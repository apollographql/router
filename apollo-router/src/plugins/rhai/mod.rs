//! Customization via Rhai.

use std::fmt;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::future::ready;
use futures::stream::once;
use http::StatusCode;
use parking_lot::Mutex;
use rhai::AST;
use rhai::Dynamic;
use rhai::Engine;
use rhai::EvalAltResult;
use rhai::FnPtr;
use rhai::FuncArgs;
use rhai::Instant;
use rhai::Map;
use rhai::Scope;
use rhai::Shared;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::util::BoxService;

use self::engine::RhaiService;
use self::engine::SharedMut;
use crate::error::Error;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::rhai::engine::OptionDance;
use crate::services::PipelineStep;

mod engine;

pub(crate) const RHAI_SPAN_NAME: &str = "rhai_plugin";

mod execution;
mod router;
mod subgraph;
mod supergraph;

/// Plugin which implements Rhai functionality
struct Rhai {
    ast: AST,
    engine: Arc<Engine>,
    scope: Arc<Mutex<Scope<'static>>>,
}

fn default_intern_strings() -> bool {
    true
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
    /// Whether to enable Rhai's internal string interning.
    ///
    /// String interning can reduce memory allocations and string comparison
    /// cost. But it also introduces synchronization overhead.
    ///
    /// Setting this to `false` can improve throughput and is recommended
    /// for workloads with many concurrent Rhai executions.
    ///
    /// Defaults to `true`.
    #[serde(default = "default_intern_strings")]
    intern_strings: bool,
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
            init.config.intern_strings,
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
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceStep {
    Router(SharedMut<router::BoxService>),
    Supergraph(SharedMut<supergraph::BoxService>),
    Execution(SharedMut<execution::BoxService>),
    Subgraph(SharedMut<subgraph::BoxService>),
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $stage: expr) => {
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
                    let result: Result<Dynamic, Box<EvalAltResult>> = execute(
                        &$rhai_service,
                        $stage,
                        &$callback,
                        (shared_request.clone(),),
                    );
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
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $stage: expr) => {
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
                .checkpoint(move |chunked_request: $base::Request|  {
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
                    let result = execute(&$rhai_service, $stage, &$callback, (shared_request.clone(),));

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
                                    $stage,
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
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $stage: expr) => {
        $borrow.replace(|service| {
            service
                .map_response(move |response: $base::Response| {
                    let shared_response = Shared::new(Mutex::new(Some(response)));
                    let result: Result<Dynamic, Box<EvalAltResult>> = execute(
                        &$rhai_service,
                        $stage,
                        &$callback,
                        (shared_response.clone(),),
                    );

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
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $stage: expr) => {
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

                    let result = execute(
                        &$rhai_service,
                        $stage,

                        &$callback,
                        (shared_response.clone(),),
                    );
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
                                $stage,
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
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident, $stage: expr) => {
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
                            message: Some(redacted_message(StatusCode::INTERNAL_SERVER_ERROR)),
                            position: None,
                            body: None,
                            internal_detail: Some("rhai execution error: empty response".to_string()),
                        };
                        tracing::error!("map_response callback failed: {error_details:#?}");
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

                    let result = execute(
                        &$rhai_service,
                        $stage,

                        &$callback,
                        (shared_response.clone(),),
                    );
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
                                $stage,
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

impl ServiceStep {
    fn map_request(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Router(service) => {
                gen_map_router_deferred_request!(
                    router,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::RouterRequest
                );
            }
            ServiceStep::Supergraph(service) => {
                gen_map_request!(
                    supergraph,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::SupergraphRequest
                );
            }
            ServiceStep::Execution(service) => {
                gen_map_request!(
                    execution,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::ExecutionRequest
                );
            }
            ServiceStep::Subgraph(service) => {
                gen_map_request!(
                    subgraph,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::SubgraphRequest
                );
            }
        }
    }

    fn map_response(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Router(service) => {
                gen_map_router_deferred_response!(
                    router,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::RouterResponse
                );
            }
            ServiceStep::Supergraph(service) => {
                gen_map_deferred_response!(
                    supergraph,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::SupergraphResponse
                );
            }
            ServiceStep::Execution(service) => {
                gen_map_deferred_response!(
                    execution,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::ExecutionResponse
                );
            }
            ServiceStep::Subgraph(service) => {
                gen_map_response!(
                    subgraph,
                    service,
                    rhai_service,
                    callback,
                    PipelineStep::SubgraphResponse
                );
            }
        }
    }
}

#[derive(Debug)]
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

/// What a failed Rhai callback turns into: the response the client gets, and the reason an operator
/// needs.
///
/// This is built by `process_error`, never deserialized from a script's value - `process_error`
/// reads the keys of a thrown object map one at a time instead, so that a key it cannot use does
/// not take the rest of the author's intent down with it.
#[derive(Debug)]
struct ErrorDetails {
    status: StatusCode,
    message: Option<String>,
    position: Option<Position>,
    body: Option<crate::graphql::Response>,
    /// The unredacted Rhai error, kept for server-side logging only.
    ///
    /// This holds Rhai implementation details - engine error text, script line numbers and the
    /// names of the callbacks involved - so it must never be copied into `message` or into a
    /// client-facing response.
    internal_detail: Option<String>,
}

/// The client-facing message for a failure the script author did not choose, i.e. anything the
/// script did not explicitly `throw`.
///
/// Returning the Rhai error itself discloses that the router runs Rhai, which script functions are
/// registered, and where in the script the failure happened, so clients get the status code's
/// reason phrase instead and the real error is logged.
fn redacted_message(status: StatusCode) -> String {
    // A script is free to throw a status code that has no reason phrase - `throw #{ status: 599 }`
    // - so there has to be a fallback. It is deliberately as vague as the status is: saying
    // "Internal Server Error" alongside a 599 would be a lie.
    status
        .canonical_reason()
        .unwrap_or("Unknown Error")
        .to_string()
}

/// Whether a thrown object map is one rhai handed a `catch` block for an engine failure, rather
/// than one the script wrote itself.
///
/// `EvalAltResult::dump_fields` stamps every such map with an `error` key naming the variant -
/// `"ErrorFunctionNotFound"` and the like. A script's own throw never picks that key up, because
/// rhai passes a thrown value to `catch` unchanged rather than rebuilding it, so the key tells the
/// two apart. It matters because the `message` on an engine map is the engine's own error text: a
/// script re-throwing that map, with or without a status set on it first, would put back exactly
/// what `process_error` redacts everywhere else.
fn is_caught_engine_error(thrown_map: &Map) -> bool {
    thrown_map
        .get("error")
        .is_some_and(|variant| variant.is_string())
}

fn process_error(error: Box<EvalAltResult>) -> ErrorDetails {
    let mut error_details = ErrorDetails {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: None,
        position: None,
        body: None,
        // Rendered before `error` is taken apart below: this is the only place the whole Rhai
        // error is available, including the chain of script callbacks it came up through.
        internal_detail: Some(format!("rhai execution error: '{error}'")),
    };

    let inner_error = error.unwrap_inner();
    // A script's `throw` is the usual source of `ErrorRuntime`, and the only source that carries a
    // message the author chose to show a client, so this is the one variant whose text can be
    // returned. Every other variant is an engine failure - unknown function, type mismatch, script
    // recursion limit - whose text describes the script's internals, so those stay redacted.
    //
    // Rhai raises `ErrorRuntime` with a string of its own in two places - a `sort` comparer that
    // fails, and a serialization failure in `to_dynamic` - and nothing in the value distinguishes
    // those from a `throw`, so they are returned like one. (Its deserialization failures in
    // `from_dynamic` are `ErrorParsing`, so they stay redacted here.) Router bindings, which we do
    // control, mark their errors instead: see `engine::internal_error`.
    if let EvalAltResult::ErrorRuntime(thrown, pos) = inner_error {
        error_details.position = Some(pos.into());

        if let Some(internal_message) = engine::internal_error_message(thrown) {
            // A router Rhai binding failed rather than the script throwing, so the message
            // describes router internals: it stays out of the response and goes to the logs. Rhai
            // renders a marked error as its Rust type rather than as its message, so put the
            // message back where that type was named - rebuilding the detail from this error alone
            // would drop the `in call to function` chain around it.
            if let Some(detail) = &mut error_details.internal_detail {
                *detail = detail.replace(engine::internal_error_displayed_as(), &internal_message);
            }
        } else if let Some(thrown_map) = thrown.read_lock::<Map>() {
            // `throw #{ status: ..., message: ..., body: ... }`, read one key at a time. A script
            // is free to throw a map carrying keys of its own - rhai gives a `catch` block `line`
            // and `position` integers alongside the message - and a key we cannot use must not
            // discard the status and message the author did choose.
            let thrown_value = |key: &str| thrown_map.get(key).filter(|value| !value.is_unit());
            if let Some(status) = thrown_value("status")
                .and_then(|value| value.as_int().ok())
                .and_then(|code| u16::try_from(code).ok())
                .and_then(|code| StatusCode::from_u16(code).ok())
            {
                error_details.status = status;
            }
            let message = thrown_value("message")
                .filter(|_| !is_caught_engine_error(&thrown_map))
                .and_then(|value| value.as_immutable_string_ref().ok())
                .map(|message| message.to_string());
            let body = thrown_value("body").and_then(|value| {
                rhai::serde::from_dynamic::<crate::graphql::Response>(value).ok()
            });
            // A throw carrying only a status - `throw #{ status: 400 }` - gets the status it asked
            // for and a redacted message, because there is no author-provided message to return.
            if message.is_some() || body.is_some() {
                error_details.message = message;
                error_details.body = body;
            }
        } else if let Ok(thrown_message) = thrown.as_immutable_string_ref() {
            // `throw "some message"`. The author wrote this string, so it is theirs to return - but
            // only the string itself, not the Rhai wrapper around it.
            error_details.message = Some(thrown_message.to_string());
        }
    }

    if error_details.message.is_none() {
        error_details.message = Some(redacted_message(error_details.status));
    }
    error_details
}

/// Execute a Rhai callback for a pipeline service stage.
///
/// Emits a metric recording the time spent executing the Rhai script.
fn execute(
    rhai_service: &RhaiService,
    stage: PipelineStep,
    callback: &FnPtr,
    args: impl FuncArgs,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let start = Instant::now();

    let result = if callback.is_curried() {
        callback.call(&rhai_service.engine, &rhai_service.ast, args)
    } else {
        let mut guard = rhai_service.scope.lock();
        rhai_service
            .engine
            .call_fn(&mut guard, &rhai_service.ast, callback.fn_name(), args)
    };

    let duration = start.elapsed();

    record_rhai_execution(stage, duration, result.is_ok());

    result
}

fn record_rhai_execution(stage: PipelineStep, duration: Duration, succeeded: bool) {
    let duration = duration.as_secs_f64();
    let stage = stage.to_string();

    f64_histogram_with_unit!(
        "apollo.router.operations.rhai.duration",
        "Time spent executing a Rhai script callback, in seconds",
        "s",
        duration,
        "rhai.stage" = stage,
        "rhai.succeeded" = succeeded
    );
}

register_plugin!("apollo", "rhai", Rhai);

#[cfg(test)]
mod tests;
