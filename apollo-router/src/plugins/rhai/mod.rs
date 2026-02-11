//! Customization via Rhai.

use std::fmt;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::StreamExt;
use futures::future::ready;
use futures::stream::once;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use http::StatusCode;
use parking_lot::Mutex;
use rhai::AST;
use rhai::Dynamic;
use rhai::Engine;
use rhai::EvalAltResult;
use rhai::FnPtr;
use rhai::FuncArgs;
use rhai::Instant;
use rhai::Scope;
use rhai::Shared;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::util::BoxService;
use tower_service::Service;

use self::engine::RhaiService;
use self::engine::SharedMut;
use crate::error::Error;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::rhai::engine::OptionDance;
use crate::register_plugin;
use crate::services::http as service_http;
use crate::services::http_layer;
use crate::services::router as services_router;

mod engine;

pub(crate) const RHAI_SPAN_NAME: &str = "rhai_plugin";

mod execution;
mod rhai_http;
mod router;
mod subgraph;
mod supergraph;

/// Plugin which implements Rhai functionality
struct Rhai {
    ast: AST,
    engine: Arc<Engine>,
    scope: Arc<Mutex<Scope<'static>>>,
}

/// Configuration for the Rhai Plugin
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "RhaiConfig")]
pub(crate) struct Conf {
    /// The directory where Rhai scripts can be found
    scripts: Option<PathBuf>,
    /// The main entry point for Rhai script evaluation
    main: Option<String>,
}

#[async_trait::async_trait]
impl Plugin for Rhai {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let sdl = init.supergraph_sdl.clone();
        let scripts_path = match init.config.scripts {
            Some(path) => path,
            None => "rhai".into(),
        };

        let main_file = match init.config.main {
            Some(main) => main,
            None => "main.rhai".to_string(),
        };

        let main = scripts_path.join(main_file);

        let engine = Arc::new(Rhai::new_rhai_engine(
            Some(scripts_path),
            sdl.to_string(),
            main.clone(),
        ));
        let ast = engine
            .compile_file(main.clone())
            .map_err(|err| format!("in Rhai script {}: {}", main.display(), err))?;
        let mut scope = Scope::new();
        // Keep these two lower cases ones as mistakes until 2.0
        // At 2.0 (or maybe before), replace with upper case
        // Note: Any constants that we add to scope here, *must* be catered for in the on_var
        // functionality in `new_rhai_engine`.
        scope.push_constant("apollo_sdl", sdl.to_string());
        scope.push_constant("apollo_start", Instant::now());

        // Run the AST with our scope to put any global variables
        // defined in scripts into scope.
        engine.run_ast_with_scope(&mut scope, &ast)?;

        Ok(Self {
            ast,
            engine,
            scope: Arc::new(Mutex::new(scope)),
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        const FUNCTION_NAME_SERVICE: &str = "router_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("router_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            None,
            ServiceStep::Router(shared_service.clone()),
            self.scope.clone(),
        ) {
            tracing::error!(
                service = "RouterService",
                "service callback failed: {error}"
            );
        }
        shared_service.take_unwrap()
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        const FUNCTION_NAME_SERVICE: &str = "supergraph_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("supergraph_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            None,
            ServiceStep::Supergraph(shared_service.clone()),
            self.scope.clone(),
        ) {
            tracing::error!(
                service = "SupergraphService",
                "service callback failed: {error}"
            );
        }
        shared_service.take_unwrap()
    }

    fn execution_service(&self, service: execution::BoxService) -> execution::BoxService {
        const FUNCTION_NAME_SERVICE: &str = "execution_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("execution_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            None,
            ServiceStep::Execution(shared_service.clone()),
            self.scope.clone(),
        ) {
            tracing::error!(
                service = "ExecutionService",
                "service callback failed: {error}"
            );
        }
        shared_service.take_unwrap()
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        const FUNCTION_NAME_SERVICE: &str = "subgraph_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("subgraph_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            Some(name),
            ServiceStep::Subgraph(shared_service.clone()),
            self.scope.clone(),
        ) {
            tracing::error!(
                service = "SubgraphService",
                subgraph = name,
                "service callback failed: {error}"
            );
        }
        shared_service.take_unwrap()
    }

    fn http_service(&self, service: http_layer::BoxService) -> http_layer::BoxService {
        const FUNCTION_NAME_SERVICE: &str = "http_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("http_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            None,
            ServiceStep::Http(shared_service.clone()),
            self.scope.clone(),
        ) {
            tracing::error!(service = "HttpService", "service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn service_http(
        &self,
        name: &str,
        service: service_http::BoxService,
    ) -> service_http::BoxService {
        const FUNCTION_NAME_SERVICE: &str = "service_http";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("service_http function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        let callbacks: SharedMut<(Option<FnPtr>, Option<FnPtr>)> =
            Arc::new(Mutex::new(Some((None, None))));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            Some(name),
            ServiceStep::ServiceHttp(
                shared_service.clone(),
                name.to_string(),
                callbacks.clone(),
            ),
            self.scope.clone(),
        ) {
            tracing::error!(service = "ServiceHttp", "service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceStep {
    Http(SharedMut<http_layer::BoxService>),
    ServiceHttp(
        SharedMut<service_http::BoxService>,
        String,
        SharedMut<(Option<FnPtr>, Option<FnPtr>)>,
    ),
    Router(SharedMut<router::BoxService>),
    Supergraph(SharedMut<supergraph::BoxService>),
    Execution(SharedMut<execution::BoxService>),
    Subgraph(SharedMut<subgraph::BoxService>),
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            fn rhai_service_span() -> impl Fn(&$base::Request) -> tracing::Span + Clone {
                move |_request: &$base::Request| {
                    tracing::info_span!(
                        RHAI_SPAN_NAME,
                        "rhai service" = stringify!($base::Request),
                        "otel.kind" = "INTERNAL"
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint(move |request: $base::Request| {
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result: Result<Dynamic, Box<EvalAltResult>> =
                        execute(&$rhai_service, &$callback, (shared_request.clone(),));
                    if let Err(error) = result {
                        let error_details = process_error(error);
                        if error_details.body.is_none() {
                            tracing::error!("map_request callback failed: {error_details:#?}");
                        }

                        let mut guard = shared_request.lock();
                        let request_opt = guard.take();
                        return $base::request_failure(request_opt.unwrap().context, error_details);
                    }
                    let mut guard = shared_request.lock();
                    let request_opt = guard.take();
                    Ok(ControlFlow::Continue(request_opt.unwrap()))
                })
                .service(service)
                .boxed()
        })
    };
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_router_deferred_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            fn rhai_service_span() -> impl Fn(&$base::Request) -> tracing::Span + Clone {
                move |_request: &$base::Request| {
                    tracing::info_span!(
                        RHAI_SPAN_NAME,
                        "rhai service" = stringify!($base::Request),
                        "otel.kind" = "INTERNAL"
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint( move |chunked_request: $base::Request|  {
                    // we split the request stream into headers+first body chunk, then a stream of chunks
                    // for which we will implement mapping later
                    let $base::Request { router_request, context } = chunked_request;
                    let (parts, stream) = router_request.into_parts();

                    let request = $base::FirstRequest {
                        context,
                        request: http::Request::from_parts(
                            parts,
                           (),
                        ),
                    };
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result = execute(&$rhai_service, &$callback, (shared_request.clone(),));

                    if let Err(error) = result {
                        let error_details = process_error(error);
                        if error_details.body.is_none() {
                            tracing::error!("map_request callback failed: {error_details:#?}");
                        }
                        let mut guard = shared_request.lock();
                        let request_opt = guard.take();
                        return $base::request_failure(request_opt.unwrap().context, error_details);
                    }

                    let request_opt = shared_request.lock().take();

                    let $base::FirstRequest { context, request } =
                    request_opt.unwrap();
                    let (parts, _body) = http::Request::from(request).into_parts();

                    // Finally, return a response which has a Body that wraps our stream of response chunks.
                    Ok(ControlFlow::Continue($base::Request {
                        context,
                        router_request: http::Request::from_parts(parts, stream),
                    }))

                    /*TODO: reenable when https://github.com/apollographql/router/issues/3642 is decided
                    let ctx = context.clone();
                    let rhai_service = $rhai_service.clone();
                    let callback = $callback.clone();

                    let mapped_stream = stream
                        .map_err(BoxError::from)
                        .and_then(move |chunk| {
                            let context = ctx.clone();
                            let rhai_service = rhai_service.clone();
                            let callback = callback.clone();
                            async move {
                                let request = $base::ChunkedRequest {
                                    context,
                                    request: chunk.into(),
                                };
                                let shared_request = Shared::new(Mutex::new(Some(request)));

                                let result = execute(
                                    &rhai_service,
                                    &callback,
                                    (shared_request.clone(),),
                                );

                                if let Err(error) = result {
                                    tracing::error!("map_request callback failed: {error}");
                                    let error_details = process_error(error);
                                    let error = Error {
                                        message: error_details.message.unwrap_or_default(),
                                        ..Default::default()
                                    };
                                    // We don't have a structured response to work with here. Let's
                                    // throw away our response and custom build an error response
                                    let error_response = graphql::Response::builder()
                                        .errors(vec![error]).build();
                                    return Ok(serde_json::to_vec(&error_response)?.into());
                                }

                                let request_opt = shared_request.lock().take();
                                let $base::ChunkedRequest { request, .. } =
                                    request_opt.unwrap();
                                Ok(request)
                            }
                        });

                    // Finally, return a response which has a Body that wraps our stream of response chunks.
                    Ok(ControlFlow::Continue($base::Request {
                        context,
                        router_request: http::Request::from_parts(parts, hyper::Body::wrap_stream(mapped_stream)),
                    }))
                    */
                })
                .service(service)
                .boxed()
        })
    };
}

macro_rules! gen_map_response {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            service
                .map_response(move |response: $base::Response| {
                    let shared_response = Shared::new(Mutex::new(Some(response)));
                    let result: Result<Dynamic, Box<EvalAltResult>> =
                        execute(&$rhai_service, &$callback, (shared_response.clone(),));

                    if let Err(error) = result {
                        let error_details = process_error(error);
                        if error_details.body.is_none() {
                            tracing::error!("map_request callback failed: {error_details:#?}");
                        }
                        let mut guard = shared_response.lock();
                        let response_opt = guard.take();
                        return $base::response_failure(
                            response_opt.unwrap().context,
                            error_details,
                        );
                    }
                    let mut guard = shared_response.lock();
                    let response_opt = guard.take();
                    response_opt.unwrap()
                })
                .boxed()
        })
    };
}

// Even though this macro is only ever used to generate router service handling, I'm leaving it as
// a macro so that the code shape is "similar" to the way in which other services are processed.
//
// I can't easily unify the macros because the router response processing is quite different to
// other service in terms of payload.
macro_rules! gen_map_router_deferred_response {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            BoxService::new(service.and_then(
                |mapped_response: $base::Response| async move {
                    // we split the response stream into headers+first response, then a stream of deferred responses
                    // for which we will implement mapping later
                    let $base::Response { response, context } = mapped_response;
                    let (parts, stream) = response.into_parts();

                    let response = $base::FirstResponse {
                        context,
                        response: http::Response::from_parts(
                            parts,
                            (),
                        )
                        .into(),
                    };
                    let shared_response = Shared::new(Mutex::new(Some(response)));

                    let result =
                        execute(&$rhai_service, &$callback, (shared_response.clone(),));
                    if let Err(error) = result {

                        let error_details = process_error(error);
                        if error_details.body.is_none() {
                            tracing::error!("map_request callback failed: {error_details:#?}");
                        }
                        let response_opt = shared_response.lock().take();
                        return Ok($base::response_failure(
                            response_opt.unwrap().context,
                            error_details
                        ));
                    }

                    let response_opt = shared_response.lock().take();

                    let $base::FirstResponse { context, response } =
                        response_opt.unwrap();
                    let (parts, _body) = http::Response::from(response).into_parts();


                    // Finally, return a response which has a Body that wraps our stream of response chunks.
                    Ok($base::Response {
                        context,
                        response: http::Response::from_parts(parts, stream),
                    })

                    /*TODO: reenable when https://github.com/apollographql/router/issues/3642 is decided
                    let ctx = context.clone();

                    let mapped_stream = rest
                        .map_err(BoxError::from)
                        .and_then(move |deferred_response| {
                        let rhai_service = $rhai_service.clone();
                        let context = ctx.clone();
                        let callback = $callback.clone();
                        async move {
                            let response = $base::DeferredResponse {
                                context,
                                response: deferred_response.into(),
                            };
                            let shared_response = Shared::new(Mutex::new(Some(response)));

                            let result = execute(
                                &rhai_service,
                                &callback,
                                (shared_response.clone(),),
                            );

                            if let Err(error) = result {
                                tracing::error!("map_response callback failed: {error}");
                                let error_details = process_error(error);
                                let error = Error {
                                    message: error_details.message.unwrap_or_default(),
                                    ..Default::default()
                                };
                                // We don't have a structured response to work with here. Let's
                                // throw away our response and custom build an error response
                                let error_response = graphql::Response::builder()
                                    .errors(vec![error]).build();
                                return Ok(serde_json::to_vec(&error_response)?.into());
                            }

                            let response_opt = shared_response.lock().take();
                            let $base::DeferredResponse { response, .. } =
                                response_opt.unwrap();
                            Ok(response)
                        }
                    });

                    // Create our response stream which consists of the bytes from our first body chained with the
                    // rest of the responses in our mapped stream.
                    let final_stream = once(ready(Ok(body))).chain(mapped_stream).boxed();

                    // Finally, return a response which has a Body that wraps our stream of response chunks.
                    Ok($base::Response {
                        context,
                        response: http::Response::from_parts(parts, hyper::Body::wrap_stream(final_stream)),
                    })*/
                },
            ))
        })
    };
}

macro_rules! gen_map_deferred_response {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            BoxService::new(service.and_then(
                |mapped_response: $base::Response| async move {
                    // we split the response stream into headers+first response, then a stream of deferred responses
                    // for which we will implement mapping later
                    let $base::Response { response, context } = mapped_response;
                    let (parts, stream) = response.into_parts();
                    let (first, rest) = StreamExt::into_future(stream).await;

                    if first.is_none() {
                        let error_details = ErrorDetails {
                            status: StatusCode::INTERNAL_SERVER_ERROR,
                            message: Some("rhai execution error: empty response".to_string()),
                            position: None,
                            body: None
                        };
                        return Ok($base::response_failure(
                            context,
                            error_details
                        ));
                    }

                    let response = $base::FirstResponse {
                        context,
                        response: http::Response::from_parts(
                            parts,
                            first.expect("already checked"),
                        )
                        .into(),
                    };
                    let shared_response = Shared::new(Mutex::new(Some(response)));

                    let result =
                        execute(&$rhai_service, &$callback, (shared_response.clone(),));
                    if let Err(error) = result {
                        let error_details = process_error(error);
                        if error_details.body.is_none() {
                            tracing::error!("map_request callback failed: {error_details:#?}");
                        }
                        let mut guard = shared_response.lock();
                        let response_opt = guard.take();
                        return Ok($base::response_failure(
                            response_opt.unwrap().context,
                            error_details
                        ));
                    }

                    let mut guard = shared_response.lock();
                    let response_opt = guard.take();
                    let $base::FirstResponse { context, response } =
                        response_opt.unwrap();
                    let (parts, body) = http::Response::from(response).into_parts();

                    let ctx = context.clone();

                    let mapped_stream = rest.filter_map(move |deferred_response| {
                        let rhai_service = $rhai_service.clone();
                        let context = context.clone();
                        let callback = $callback.clone();
                        async move {
                            let response = $base::DeferredResponse {
                                context,
                                response: deferred_response,
                            };
                            let shared_response = Shared::new(Mutex::new(Some(response)));

                            let result = execute(
                                &rhai_service,
                                &callback,
                                (shared_response.clone(),),
                            );
                            if let Err(error) = result {
                                let error_details = process_error(error);
                                if error_details.body.is_none() {
                                    tracing::error!("map_request callback failed: {error_details:#?}");
                                }
                                let mut guard = shared_response.lock();
                                let response_opt = guard.take();
                                let $base::DeferredResponse { mut response, .. } = response_opt.unwrap();
                                let error = Error::builder()
                                    .message(error_details.message.unwrap_or_default())
                                    .build();
                                response.errors = vec![error];
                                return Some(response);
                            }

                            let mut guard = shared_response.lock();
                            let response_opt = guard.take();
                            let $base::DeferredResponse { response, .. } =
                                response_opt.unwrap();
                            Some(response)
                        }
                    });

                    let response = http::Response::from_parts(
                        parts,
                        once(ready(body)).chain(mapped_stream).boxed(),
                    )
                    .into();
                    Ok($base::Response {
                        context: ctx,
                        response,
                    })
                },
            ))
        })
    };
}

/// Message sent to the task that owns the inner service.
type ServiceHttpTaskMessage = (
    service_http::HttpRequest,
    oneshot::Sender<Result<service_http::HttpResponse, BoxError>>,
);

/// Wrapper service that runs the Rhai map_request callback for service_http.
/// Buffers the request body, runs the script, then sends to a task that calls the inner service.
struct ServiceHttpRequestLayer {
    request_tx: mpsc::Sender<ServiceHttpTaskMessage>,
    callbacks: SharedMut<(Option<FnPtr>, Option<FnPtr>)>,
    rhai_service: RhaiService,
    service_name: String,
}

impl Service<service_http::HttpRequest> for ServiceHttpRequestLayer {
    type Response = service_http::HttpResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: service_http::HttpRequest) -> Self::Future {
        let request_tx = self.request_tx.clone();
        let callbacks = self.callbacks.clone();
        let rhai_service = self.rhai_service.clone();
        let service_name = self.service_name.clone();
        Box::pin(async move {
            let (parts, body) = req.http_request.into_parts();
            let body_bytes = services_router::body::into_bytes(body)
                .await
                .map_err(|e| -> BoxError { e.into() })?;
            let req_cb = callbacks.with_mut(|c| c.0.clone());
            let modified = if let Some(callback) = req_cb {
                let wrapper = rhai_http::RhaiServiceHttpRequest::from_parts(
                    parts,
                    body_bytes,
                    &req.context,
                    Some(service_name),
                );
                let shared = Shared::new(Mutex::new(Some(wrapper)));
                let exec_result: Result<Dynamic, Box<EvalAltResult>> =
                    execute(&rhai_service, &callback, (shared.clone(),));
                if let Err(error) = exec_result {
                    let error_details = process_error(error);
                    if error_details.body.is_none() {
                        tracing::error!(
                            "service_http map_request callback failed: {error_details:#?}"
                        );
                    }
                    return Ok(rhai_http::service_http_response_failure(
                        req.context,
                        error_details,
                    ));
                }
                shared.lock().take().unwrap().into_http_request(req.context)
            } else {
                let http_request = http::Request::from_parts(
                    parts,
                    services_router::body::from_bytes(body_bytes),
                );
                service_http::HttpRequest {
                    http_request,
                    context: req.context,
                }
            };
            let (resp_tx, resp_rx) = oneshot::channel();
            if request_tx.send((modified, resp_tx)).await.is_err() {
                return Err("service HTTP request layer: task closed".into());
            }
            match resp_rx.await {
                Ok(Ok(r)) => Ok(r),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("service HTTP request layer: channel closed".into()),
            }
        })
    }
}

/// Wrapper service that runs the Rhai map_response callback for service_http.
/// Sends to a task that calls the inner service, then buffers the response and runs the script.
struct ServiceHttpResponseLayer {
    request_tx: mpsc::Sender<ServiceHttpTaskMessage>,
    callbacks: SharedMut<(Option<FnPtr>, Option<FnPtr>)>,
    rhai_service: RhaiService,
}

impl Service<service_http::HttpRequest> for ServiceHttpResponseLayer {
    type Response = service_http::HttpResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: service_http::HttpRequest) -> Self::Future {
        let request_tx = self.request_tx.clone();
        let callbacks = self.callbacks.clone();
        let rhai_service = self.rhai_service.clone();
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            if request_tx.send((req, resp_tx)).await.is_err() {
                return Err("service HTTP response layer: task closed".into());
            }
            let response = match resp_rx.await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err("service HTTP response layer: channel closed".into()),
            };
            let res_cb = callbacks.with_mut(|c| c.1.clone());
            let modified = if let Some(callback) = res_cb {
                let (parts, body) = response.http_response.into_parts();
                let body_bytes = services_router::body::into_bytes(body)
                    .await
                    .map_err(|e| -> BoxError { e.into() })?;
                let wrapper =
                    rhai_http::RhaiServiceHttpResponse::from_parts(parts, body_bytes);
                let shared = Shared::new(Mutex::new(Some(wrapper)));
                let exec_result: Result<Dynamic, Box<EvalAltResult>> =
                    execute(&rhai_service, &callback, (shared.clone(),));
                if let Err(error) = exec_result {
                    let error_details = process_error(error);
                    if error_details.body.is_none() {
                        tracing::error!(
                            "service_http map_response callback failed: {error_details:#?}"
                        );
                    }
                    return Ok(rhai_http::service_http_response_failure(
                        response.context,
                        error_details,
                    ));
                }
                shared.lock().take().unwrap().into_http_response(response.context)
            } else {
                response
            };
            Ok(modified)
        })
    }
}

impl ServiceStep {
    fn map_request(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Http(service) => {
                service.replace(|inner| {
                    let rhai_service = rhai_service.clone();
                    ServiceBuilder::new()
                        .checkpoint(move |request: http_layer::HttpRequest| {
                            let wrapper = rhai_http::RhaiHttpRequest::from_http_request(request);
                            let shared_request = Shared::new(Mutex::new(Some(wrapper)));
                            let result: Result<Dynamic, Box<EvalAltResult>> =
                                execute(&rhai_service, &callback, (shared_request.clone(),));
                            if let Err(error) = result {
                                let error_details = process_error(error);
                                if error_details.body.is_none() {
                                    tracing::error!("map_request callback failed: {error_details:#?}");
                                }
                                return Ok(rhai_http::request_failure(error_details)
                                    .unwrap_or_else(|_| {
                                        ControlFlow::Break(
                                            http::Response::builder()
                                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                                .body(bytes::Bytes::from(
                                                    "{\"errors\":[{\"message\":\"internal error\"}]}",
                                                ))
                                                .expect("valid response"),
                                        )
                                    }));
                            }
                            let request_opt = shared_request.lock().take();
                            Ok(ControlFlow::Continue(
                                request_opt.unwrap().into_http_request(),
                            ))
                        })
                        .service(inner)
                        .boxed()
                });
            }
            ServiceStep::Router(service) => {
                gen_map_router_deferred_request!(router, service, rhai_service, callback);
            }
            ServiceStep::Supergraph(service) => {
                gen_map_request!(supergraph, service, rhai_service, callback);
            }
            ServiceStep::Execution(service) => {
                gen_map_request!(execution, service, rhai_service, callback);
            }
            ServiceStep::Subgraph(service) => {
                gen_map_request!(subgraph, service, rhai_service, callback);
            }
            ServiceStep::ServiceHttp(service, service_name, callbacks) => {
                callbacks.with_mut(|c| c.0 = Some(callback));
                service.replace(|mut inner| {
                    let (tx, mut rx) = mpsc::channel::<ServiceHttpTaskMessage>(32);
                    tokio::spawn(async move {
                        while let Some((req, resp_tx)) = rx.recv().await {
                            let result = inner.call(req).await;
                            let _ = resp_tx.send(result);
                        }
                    });
                    BoxService::new(ServiceHttpRequestLayer {
                        request_tx: tx,
                        callbacks: callbacks.clone(),
                        rhai_service: rhai_service.clone(),
                        service_name: service_name.clone(),
                    })
                });
            }
        }
    }

    fn map_response(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Http(service) => {
                service.replace(|inner| {
                    let rhai_service = rhai_service.clone();
                    tower::ServiceExt::map_response(
                        inner,
                        move |response: http_layer::HttpResponse| {
                            let wrapper = rhai_http::RhaiHttpResponse::from_http_response(response);
                            let shared_response = Shared::new(Mutex::new(Some(wrapper)));
                            let result: Result<Dynamic, Box<EvalAltResult>> =
                                execute(&rhai_service, &callback, (shared_response.clone(),));
                            if let Err(error) = result {
                                let error_details = process_error(error);
                                if error_details.body.is_none() {
                                    tracing::error!(
                                        "map_response callback failed: {error_details:#?}"
                                    );
                                }
                                return rhai_http::response_failure(error_details);
                            }
                            shared_response.lock().take().unwrap().into_http_response()
                        },
                    )
                    .boxed()
                });
            }
            ServiceStep::Router(service) => {
                gen_map_router_deferred_response!(router, service, rhai_service, callback);
            }
            ServiceStep::Supergraph(service) => {
                gen_map_deferred_response!(supergraph, service, rhai_service, callback);
            }
            ServiceStep::Execution(service) => {
                gen_map_deferred_response!(execution, service, rhai_service, callback);
            }
            ServiceStep::Subgraph(service) => {
                gen_map_response!(subgraph, service, rhai_service, callback);
            }
            ServiceStep::ServiceHttp(service, _service_name, callbacks) => {
                callbacks.with_mut(|c| c.1 = Some(callback));
                service.replace(|mut inner| {
                    let (tx, mut rx) = mpsc::channel::<ServiceHttpTaskMessage>(32);
                    tokio::spawn(async move {
                        while let Some((req, resp_tx)) = rx.recv().await {
                            let result = inner.call(req).await;
                            let _ = resp_tx.send(result);
                        }
                    });
                    BoxService::new(ServiceHttpResponseLayer {
                        request_tx: tx,
                        callbacks: callbacks.clone(),
                        rhai_service: rhai_service.clone(),
                    })
                });
            }
        }
    }
}

#[derive(Deserialize, Debug)]
struct Position {
    line: Option<usize>,
    pos: Option<usize>,
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((line, pos)) = self.line.zip(self.pos) {
            write!(f, "line {line}, position {pos}")
        } else {
            write!(f, "none")
        }
    }
}

impl From<&rhai::Position> for Position {
    fn from(value: &rhai::Position) -> Self {
        Self {
            line: value.line(),
            pos: value.position(),
        }
    }
}

#[derive(Deserialize, Debug)]
struct ErrorDetails {
    #[serde(
        with = "http_serde::status_code",
        default = "default_thrown_status_code"
    )]
    status: StatusCode,
    message: Option<String>,
    position: Option<Position>,
    body: Option<crate::graphql::Response>,
}

fn default_thrown_status_code() -> StatusCode {
    StatusCode::INTERNAL_SERVER_ERROR
}

fn process_error(error: Box<EvalAltResult>) -> ErrorDetails {
    let mut error_details = ErrorDetails {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: Some(format!("rhai execution error: '{error}'")),
        position: None,
        body: None,
    };

    let inner_error = error.unwrap_inner();
    // We only want to process runtime errors
    if let EvalAltResult::ErrorRuntime(obj, pos) = inner_error {
        if let Ok(temp_error_details) = rhai::serde::from_dynamic::<ErrorDetails>(obj) {
            if temp_error_details.message.is_some() || temp_error_details.body.is_some() {
                error_details = temp_error_details;
            } else {
                error_details.status = temp_error_details.status;
            }
        }
        error_details.position = Some(pos.into());
    }
    error_details
}

fn execute(
    rhai_service: &RhaiService,
    callback: &FnPtr,
    args: impl FuncArgs,
) -> Result<Dynamic, Box<EvalAltResult>> {
    if callback.is_curried() {
        callback.call(&rhai_service.engine, &rhai_service.ast, args)
    } else {
        let mut guard = rhai_service.scope.lock();
        rhai_service
            .engine
            .call_fn(&mut guard, &rhai_service.ast, callback.fn_name(), args)
    }
}

register_plugin!("apollo", "rhai", Rhai);

#[cfg(test)]
mod tests;
