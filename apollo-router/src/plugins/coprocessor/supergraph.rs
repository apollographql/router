use std::ops::ControlFlow;
use std::sync::Arc;

use futures::future;
use futures::stream;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower_service::Service;

use super::externalize_header_map;
use super::*;
use crate::graphql;
use crate::layers::async_checkpoint::OneShotAsyncCheckpointLayer;
use crate::layers::ServiceBuilderExt;
use crate::plugins::coprocessor::EXTERNAL_SPAN_NAME;
use crate::response;
use crate::services::supergraph;

/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SupergraphRequestConf {
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: bool,
    /// Send the body
    pub(super) body: bool,
    /// Send the SDL
    pub(super) sdl: bool,
    /// Send the method
    pub(super) method: bool,
}

/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SupergraphResponseConf {
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: bool,
    /// Send the body
    pub(super) body: bool,
    /// Send the SDL
    pub(super) sdl: bool,
    /// Send the HTTP status
    pub(super) status_code: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(default)]
pub(super) struct SupergraphStage {
    /// The request configuration
    pub(super) request: SupergraphRequestConf,
    // /// The response configuration
    pub(super) response: SupergraphResponseConf,
}

impl SupergraphStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: supergraph::BoxService,
        coprocessor_url: String,
        sdl: Arc<String>,
    ) -> supergraph::BoxService
    where
        C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let coprocessor_url = coprocessor_url.clone();
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            OneShotAsyncCheckpointLayer::new(move |request: supergraph::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let sdl = sdl.clone();

                async move {
                    let mut succeeded = true;
                    let result = process_supergraph_request_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        request,
                        request_config,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!(
                            "external extensibility: supergraph request stage error: {error}"
                        );
                        error
                    });
                    tracing::info!(
                        monotonic_counter.apollo.router.operations.coprocessor = 1u64,
                        coprocessor.stage = %PipelineStep::SupergraphRequest,
                        coprocessor.succeeded = succeeded,
                        "Total operations with co-processors enabled"
                    );
                    result
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();

            MapFutureLayer::new(move |fut| {
                let coprocessor_url = coprocessor_url.clone();
                let sdl: Arc<String> = sdl.clone();
                let http_client = http_client.clone();
                let response_config = response_config.clone();

                async move {
                    let response: supergraph::Response = fut.await?;

                    let mut succeeded = true;
                    let result = process_supergraph_response_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        response,
                        response_config,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!(
                            "external extensibility: router response stage error: {error}"
                        );
                        error
                    });
                    tracing::info!(
                        monotonic_counter.apollo.router.operations.coprocessor = 1u64,
                        coprocessor.stage = %PipelineStep::SupergraphResponse,
                        coprocessor.succeeded = succeeded,
                        "Total operations with co-processors enabled"
                    );
                    result
                }
            })
        });

        fn external_service_span() -> impl Fn(&supergraph::Request) -> tracing::Span + Clone {
            move |_request: &supergraph::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(supergraph::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            .option_layer(response_layer)
            .service(service)
            .boxed()
    }
}

async fn process_supergraph_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    mut request: supergraph::Request,
    request_config: SupergraphRequestConf,
) -> Result<ControlFlow<supergraph::Response, supergraph::Request>, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
{
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.supergraph_request.into_parts();
    let bytes = Bytes::from(serde_json::to_vec(&body)?);

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;

    let body_to_send = request_config
        .body
        .then(|| serde_json::from_slice::<serde_json::Value>(&bytes))
        .transpose()?;
    let context_to_send = request_config.context.then(|| request.context.clone());
    let sdl_to_send = request_config.sdl.then(|| sdl.clone().to_string());
    let method = request_config.method.then(|| parts.method.to_string());

    let payload = Externalizable::supergraph_builder()
        .stage(PipelineStep::SupergraphRequest)
        .control(Control::default())
        .and_id(TraceId::maybe_new().map(|id| id.to_string()))
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_method(method)
        .and_sdl(sdl_to_send)
        .build();

    tracing::debug!(?payload, "externalized output");
    let guard = request.context.enter_active_request();
    let start = Instant::now();
    let co_processor_result = payload.call(http_client, &coprocessor_url).await;
    let duration = start.elapsed().as_secs_f64();
    drop(guard);
    tracing::info!(
        histogram.apollo.router.operations.coprocessor.duration = duration,
        coprocessor.stage = %PipelineStep::SupergraphRequest,
    );

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;
    validate_coprocessor_output(&co_processor_output, PipelineStep::SupergraphRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    // Thirdly, we need to interpret the control flow which may have been
    // updated by our co-processor and decide if we should proceed or stop.

    if matches!(control, Control::Break(_)) {
        // Ensure the code is a valid http status code
        let code = control.get_http_status()?;

        let res = {
            let graphql_response: crate::graphql::Response =
                serde_json::from_value(co_processor_output.body.unwrap_or(serde_json::Value::Null))
                    .unwrap_or_else(|error| {
                        crate::graphql::Response::builder()
                            .errors(vec![Error::builder()
                                .message(format!(
                                    "couldn't deserialize coprocessor output body: {error}"
                                ))
                                .extension_code("EXTERNAL_DESERIALIZATION_ERROR")
                                .build()])
                            .build()
                    });

            let mut http_response = http::Response::builder()
                .status(code)
                .body(stream::once(future::ready(graphql_response)).boxed())?;
            if let Some(headers) = co_processor_output.headers {
                *http_response.headers_mut() = internalize_header_map(headers)?;
            }

            let supergraph_response = supergraph::Response {
                response: http_response,
                context: request.context,
            };

            if let Some(context) = co_processor_output.context {
                for (key, value) in context.try_into_iter()? {
                    supergraph_response
                        .context
                        .upsert_json_value(key, move |_current| value);
                }
            }

            supergraph_response
        };
        return Ok(ControlFlow::Break(res));
    }

    // Finally, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming request with the updated bits if they
    // are present in our co_processor_output.

    let new_body: crate::graphql::Request = match co_processor_output.body {
        Some(value) => serde_json::from_value(value)?,
        None => body,
    };

    request.supergraph_request = http::Request::from_parts(parts, new_body);

    if let Some(context) = co_processor_output.context {
        for (key, value) in context.try_into_iter()? {
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *request.supergraph_request.headers_mut() = internalize_header_map(headers)?;
    }

    if let Some(uri) = co_processor_output.uri {
        *request.supergraph_request.uri_mut() = uri.parse()?;
    }

    Ok(ControlFlow::Continue(request))
}

async fn process_supergraph_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    response: supergraph::Response,
    response_config: SupergraphResponseConf,
) -> Result<supergraph::Response, BoxError>
where
    C: Service<hyper::Request<Body>, Response = hyper::Response<Body>, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<http::Request<Body>>>::Future: Send + 'static,
{
    // split the response into parts + body
    let (mut parts, body) = response.response.into_parts();

    // we split the body (which is a stream) into first response + rest of responses,
    // for which we will implement mapping later
    let (first, rest): (Option<response::Response>, graphql::ResponseStream) =
        body.into_future().await;

    // If first is None, we return an error
    let first = first.ok_or_else(|| {
        BoxError::from("Coprocessor cannot convert body into future due to problem with first part")
    })?;

    // Now we process our first chunk of response
    // Encode headers, body, status, context, sdl to create a payload
    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers))
        .transpose()?;
    let body_to_send = response_config
        .body
        .then(|| serde_json::to_value(&first).expect("serialization will not fail"));
    let status_to_send = response_config.status_code.then(|| parts.status.as_u16());
    let context_to_send = response_config.context.then(|| response.context.clone());
    let sdl_to_send = response_config.sdl.then(|| sdl.clone().to_string());

    let payload = Externalizable::supergraph_builder()
        .stage(PipelineStep::SupergraphResponse)
        .and_id(TraceId::maybe_new().map(|id| id.to_string()))
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_sdl(sdl_to_send.clone())
        .build();

    // Second, call our co-processor and get a reply.
    tracing::debug!(?payload, "externalized output");
    let guard = response.context.enter_active_request();
    let start = Instant::now();
    let co_processor_result = payload.call(http_client.clone(), &coprocessor_url).await;
    let duration = start.elapsed().as_secs_f64();
    drop(guard);
    tracing::info!(
        histogram.apollo.router.operations.coprocessor.duration = duration,
        coprocessor.stage = %PipelineStep::SupergraphResponse,
    );

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::SupergraphResponse)?;

    // Third, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming response with the updated bits if they
    // are present in our co_processor_output. If they aren't present, just use the
    // bits that we sent to the co_processor.
    let new_body: crate::response::Response = match co_processor_output.body {
        Some(value) => serde_json::from_value(value)?,
        None => first,
    };

    if let Some(control) = co_processor_output.control {
        parts.status = control.get_http_status()?
    }

    if let Some(context) = co_processor_output.context {
        for (key, value) in context.try_into_iter()? {
            response
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        parts.headers = internalize_header_map(headers)?;
    }

    // Clone all the bits we need
    let context = response.context.clone();
    let map_context = response.context.clone();

    // Map the rest of our body to process subsequent chunks of response
    let mapped_stream = rest
        .then(move |deferred_response| {
            let generator_client = http_client.clone();
            let generator_coprocessor_url = coprocessor_url.clone();
            let generator_map_context = map_context.clone();
            let generator_sdl_to_send = sdl_to_send.clone();

            async move {
                let body_to_send = response_config.body.then(|| {
                    serde_json::to_value(&deferred_response).expect("serialization will not fail")
                });
                let context_to_send = response_config
                    .context
                    .then(|| generator_map_context.clone());

                // Note: We deliberately DO NOT send headers or status_code even if the user has
                // requested them. That's because they are meaningless on a deferred response and
                // providing them will be a source of confusion.
                let payload = Externalizable::router_builder()
                    .stage(PipelineStep::SupergraphResponse)
                    .and_id(TraceId::maybe_new().map(|id| id.to_string()))
                    .and_body(body_to_send)
                    .and_context(context_to_send)
                    .and_sdl(generator_sdl_to_send)
                    .build();

                // Second, call our co-processor and get a reply.
                tracing::debug!(?payload, "externalized output");
                let guard = generator_map_context.enter_active_request();
                let co_processor_result = payload
                    .call(generator_client, &generator_coprocessor_url)
                    .await;
                drop(guard);
                tracing::debug!(?co_processor_result, "co-processor returned");
                let co_processor_output = co_processor_result?;

                validate_coprocessor_output(
                    &co_processor_output,
                    PipelineStep::SupergraphResponse,
                )?;

                // Third, process our reply and act on the contents. Our processing logic is
                // that we replace "bits" of our incoming response with the updated bits if they
                // are present in our co_processor_output. If they aren't present, just use the
                // bits that we sent to the co_processor.
                let new_deferred_response: crate::response::Response =
                    match co_processor_output.body {
                        Some(value) => serde_json::from_value(value)?,
                        None => deferred_response,
                    };

                if let Some(context) = co_processor_output.context {
                    for (key, value) in context.try_into_iter()? {
                        generator_map_context.upsert_json_value(key, move |_current| value);
                    }
                }

                // We return the deferred_response into our stream of response chunks
                Ok(new_deferred_response)
            }
        })
        .map(|res: Result<response::Response, BoxError>| match res {
            Ok(response) => response,
            Err(e) => {
                tracing::error!("coprocessor error handling deferred supergraph response: {e}");
                response::Response::builder()
                    .error(
                        Error::builder()
                            .message("Internal error handling deferred response")
                            .extension_code("INTERNAL_ERROR")
                            .build(),
                    )
                    .build()
            }
        });

    // Create our response stream which consists of our first body chained with the
    // rest of the responses in our mapped stream.
    let stream = once(ready(new_body)).chain(mapped_stream).boxed();

    // Finally, return a response which has a Body that wraps our stream of response chunks.
    Ok(supergraph::Response {
        context,
        response: http::Response::from_parts(parts, stream),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;
    use http::StatusCode;
    use hyper::Body;
    use serde_json::json;
    use tower::BoxError;
    use tower::ServiceExt;

    use super::super::*;
    use super::*;
    use crate::plugin::test::MockHttpClientService;
    use crate::plugin::test::MockSupergraphService;
    use crate::services::supergraph;

    #[allow(clippy::type_complexity)]
    pub(crate) fn mock_with_callback(
        callback: fn(
            hyper::Request<Body>,
        ) -> BoxFuture<'static, Result<hyper::Response<Body>, BoxError>>,
    ) -> MockHttpClientService {
        let mut mock_http_client = MockHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockHttpClientService::new();

            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockHttpClientService::new();
                mock_http_client.expect_call().returning(callback);
                mock_http_client
            });
            mock_http_client
        });

        mock_http_client
    }

    #[allow(clippy::type_complexity)]
    fn mock_with_deferred_callback(
        callback: fn(
            hyper::Request<Body>,
        ) -> BoxFuture<'static, Result<hyper::Response<Body>, BoxError>>,
    ) -> MockHttpClientService {
        let mut mock_http_client = MockHttpClientService::new();
        mock_http_client.expect_clone().returning(move || {
            let mut mock_http_client = MockHttpClientService::new();
            mock_http_client.expect_clone().returning(move || {
                let mut mock_http_client = MockHttpClientService::new();
                mock_http_client.expect_clone().returning(move || {
                    let mut mock_http_client = MockHttpClientService::new();
                    mock_http_client.expect_call().returning(callback);
                    mock_http_client
                });
                mock_http_client
            });
            mock_http_client
        });

        mock_http_client
    }

    #[tokio::test]
    async fn external_plugin_supergraph_request() {
        let supergraph_stage = SupergraphStage {
            request: SupergraphRequestConf {
                headers: false,
                context: false,
                body: true,
                sdl: false,
                method: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mut mock_supergraph_service = MockSupergraphService::new();

        mock_supergraph_service
            .expect_call()
            .returning(|req: supergraph::Request| {
                // Let's assert that the subgraph request has been transformed as it should have.
                assert_eq!(
                    req.supergraph_request.headers().get("cookie").unwrap(),
                    "tasty_cookie=strawberry"
                );

                assert_eq!(
                    req.context
                        .get::<&str, u8>("this-is-a-test-context")
                        .unwrap()
                        .unwrap(),
                    42
                );

                // The subgraph uri should have changed
                assert_eq!(
                    Some("MyQuery"),
                    req.supergraph_request.body().operation_name.as_deref()
                );

                // The query should have changed
                assert_eq!(
                    "query Long {\n  me {\n  name\n}\n}",
                    req.supergraph_request.body().query.as_ref().unwrap()
                );

                Ok(supergraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(crate::json_ext::Object::new())
                    .context(req.context)
                    .build()
                    .unwrap())
            });

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r#"{
                                "version": 1,
                                "stage": "SupergraphRequest",
                                "control": "continue",
                                "headers": {
                                    "cookie": [
                                      "tasty_cookie=strawberry"
                                    ],
                                    "content-type": [
                                      "application/json"
                                    ],
                                    "host": [
                                      "127.0.0.1:4000"
                                    ],
                                    "apollo-federation-include-trace": [
                                      "ftv1"
                                    ],
                                    "apollographql-client-name": [
                                      "manual"
                                    ],
                                    "accept": [
                                      "*/*"
                                    ],
                                    "user-agent": [
                                      "curl/7.79.1"
                                    ],
                                    "content-length": [
                                      "46"
                                    ]
                                  },
                                  "body": {
                                    "query": "query Long {\n  me {\n  name\n}\n}",
                                    "operationName": "MyQuery"
                                  },
                                  "context": {
                                    "entries": {
                                      "accepts-json": false,
                                      "accepts-wildcard": true,
                                      "accepts-multipart": false,
                                      "this-is-a-test-context": 42
                                    }
                                  },
                                  "serviceName": "service name shouldn't change",
                                  "uri": "http://thisurihaschanged"
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = supergraph_stage.as_service(
            mock_http_client,
            mock_supergraph_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::fake_builder().build().unwrap();

        assert_eq!(
            serde_json_bytes::json!({ "test": 1234_u32 }),
            service
                .oneshot(request)
                .await
                .unwrap()
                .response
                .into_body()
                .next()
                .await
                .unwrap()
                .data
                .unwrap()
        );
    }

    #[tokio::test]
    async fn external_plugin_supergraph_request_controlflow_break() {
        let supergraph_stage = SupergraphStage {
            request: SupergraphRequestConf {
                headers: false,
                context: false,
                body: true,
                sdl: false,
                method: false,
            },
            response: Default::default(),
        };

        // This will never be called because we will fail at the coprocessor.
        let mock_supergraph_service = MockSupergraphService::new();

        let mock_http_client = mock_with_callback(move |_: hyper::Request<Body>| {
            Box::pin(async {
                Ok(hyper::Response::builder()
                    .body(Body::from(
                        r#"{
                                "version": 1,
                                "stage": "SupergraphRequest",
                                "control": {
                                    "break": 200
                                },
                                "body": {
                                    "errors": [{ "message": "my error message" }]
                                },
                                "context": {
                                    "entries": {
                                        "testKey": true
                                    }
                                },
                                "headers": {
                                    "aheader": ["a value"]
                                }
                            }"#,
                    ))
                    .unwrap())
            })
        });

        let service = supergraph_stage.as_service(
            mock_http_client,
            mock_supergraph_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::fake_builder().build().unwrap();

        let crate::services::supergraph::Response {
            mut response,
            context,
        } = service.oneshot(request).await.unwrap();

        assert!(context.get::<_, bool>("testKey").unwrap().unwrap());

        let value = response.headers().get("aheader").unwrap();

        assert_eq!("a value", value);

        assert_eq!(
            "my error message",
            response.body_mut().next().await.unwrap().errors[0]
                .message
                .as_str()
        );
    }

    #[tokio::test]
    async fn external_plugin_supergraph_response() {
        let supergraph_stage = SupergraphStage {
            response: SupergraphResponseConf {
                headers: true,
                context: true,
                body: true,
                sdl: true,
                status_code: false,
            },
            request: Default::default(),
        };

        let mut mock_supergraph_service = MockSupergraphService::new();

        mock_supergraph_service
            .expect_call()
            .returning(|req: supergraph::Request| {
                Ok(supergraph::Response::builder()
                    .data(json!({ "test": 1234_u32 }))
                    .errors(Vec::new())
                    .extensions(crate::json_ext::Object::new())
                    .context(req.context)
                    .build()
                    .unwrap())
            });

        let mock_http_client = mock_with_deferred_callback(move |res: hyper::Request<Body>| {
            Box::pin(async {
                let deserialized_response: Externalizable<serde_json::Value> =
                    serde_json::from_slice(&hyper::body::to_bytes(res.into_body()).await.unwrap())
                        .unwrap();

                assert_eq!(EXTERNALIZABLE_VERSION, deserialized_response.version);
                assert_eq!(
                    PipelineStep::SupergraphResponse.to_string(),
                    deserialized_response.stage
                );

                assert_eq!(
                    json! {{"data":{ "test": 1234_u32 }}},
                    deserialized_response.body.unwrap()
                );

                let input = json!(
                      {
                  "version": 1,
                  "stage": "SupergraphResponse",
                  "control": {
                      "break": 400
                  },
                  "id": "1b19c05fdafc521016df33148ad63c1b",
                  "headers": {
                    "cookie": [
                      "tasty_cookie=strawberry"
                    ],
                    "content-type": [
                      "application/json"
                    ],
                    "host": [
                      "127.0.0.1:4000"
                    ],
                    "apollo-federation-include-trace": [
                      "ftv1"
                    ],
                    "apollographql-client-name": [
                      "manual"
                    ],
                    "accept": [
                      "*/*"
                    ],
                    "user-agent": [
                      "curl/7.79.1"
                    ],
                    "content-length": [
                      "46"
                    ]
                  },
                  "body": {
                    "data": { "test": 42 }
                  },
                  "context": {
                    "entries": {
                      "accepts-json": false,
                      "accepts-wildcard": true,
                      "accepts-multipart": false,
                      "this-is-a-test-context": 42
                    }
                  },
                  "sdl": "the sdl shouldnt change"
                });
                Ok(hyper::Response::builder()
                    .body(Body::from(serde_json::to_string(&input).unwrap()))
                    .unwrap())
            })
        });

        let service = supergraph_stage.as_service(
            mock_http_client,
            mock_supergraph_service.boxed(),
            "http://test".to_string(),
            Arc::new("".to_string()),
        );

        let request = supergraph::Request::canned_builder().build().unwrap();

        let mut res = service.oneshot(request).await.unwrap();

        // Let's assert that the router request has been transformed as it should have.
        assert_eq!(res.response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.response.headers().get("cookie").unwrap(),
            "tasty_cookie=strawberry"
        );

        assert_eq!(
            res.context
                .get::<&str, u8>("this-is-a-test-context")
                .unwrap()
                .unwrap(),
            42
        );

        let body = res.response.body_mut().next().await.unwrap();
        // the body should have changed:
        assert_eq!(
            json!({ "data": { "test": 42_u32 } }),
            serde_json::to_value(&body).unwrap(),
        );
    }
}
