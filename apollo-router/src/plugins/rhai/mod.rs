//! Customization via Rhai.

use std::fmt;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use http::StatusCode;
use notify::event::DataChange;
use notify::event::MetadataKind;
use notify::event::ModifyKind;
use notify::Config;
use notify::EventKind;
use notify::PollWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use rhai::Dynamic;
use rhai::Engine;
use rhai::EvalAltResult;
use rhai::FnPtr;
use rhai::FuncArgs;
use rhai::Instant;
use rhai::Scope;
use rhai::Shared;
use rhai::AST;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use self::engine::RhaiService;
use self::engine::SharedMut;
use crate::error::Error;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::rhai::engine::OptionDance;
use crate::register_plugin;

mod engine;

pub(crate) const RHAI_SPAN_NAME: &str = "rhai_plugin";

mod execution;
mod router;
mod subgraph;
mod supergraph;

struct EngineBlock {
    ast: AST,
    engine: Arc<Engine>,
    scope: Arc<Mutex<Scope<'static>>>,
}

impl EngineBlock {
    fn try_new(
        scripts: Option<PathBuf>,
        main: PathBuf,
        sdl: Arc<String>,
    ) -> Result<Self, BoxError> {
        let engine = Arc::new(Rhai::new_rhai_engine(
            scripts,
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

        Ok(EngineBlock {
            ast,
            engine,
            scope: Arc::new(Mutex::new(scope)),
        })
    }
}

/// Plugin which implements Rhai functionality
/// Note: We use ArcSwap here in preference to a shared RwLock. Updates to
/// the engine block will be infrequent in relation to the accesses of it.
/// We'd love to use AtomicArc if such a thing existed, but since it doesn't
/// we'll use ArcSwap to accomplish our goal.
struct Rhai {
    block: Arc<ArcSwap<EngineBlock>>,
    park_flag: Arc<AtomicBool>,
    watcher_handle: Option<std::thread::JoinHandle<()>>,
}

/// Configuration for the Rhai Plugin
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

        let watched_path = scripts_path.clone();
        let watched_main = main.clone();
        let watched_sdl = sdl.clone();

        let block = Arc::new(ArcSwap::from_pointee(EngineBlock::try_new(
            Some(scripts_path),
            main,
            sdl,
        )?));
        let watched_block = block.clone();

        let park_flag = Arc::new(AtomicBool::new(false));
        let watching_flag = park_flag.clone();

        let watcher_handle = std::thread::spawn(move || {
            let watching_path = watched_path.clone();
            let config = Config::default()
                .with_poll_interval(Duration::from_secs(3))
                .with_compare_contents(true);
            let mut watcher = PollWatcher::new(
                move |res: Result<notify::Event, notify::Error>| {
                    match res {
                        Ok(event) => {
                            // Let's limit the events we are interested in to:
                            //  - Modified files
                            //  - Created/Remove files
                            //  - with suffix "rhai"
                            if matches!(
                                event.kind,
                                EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime))
                                    | EventKind::Modify(ModifyKind::Data(DataChange::Any))
                                    | EventKind::Create(_)
                                    | EventKind::Remove(_)
                            ) {
                                let mut proceed = false;
                                for path in event.paths {
                                    if path.extension().map_or(false, |ext| ext == "rhai") {
                                        proceed = true;
                                        break;
                                    }
                                }

                                if proceed {
                                    match EngineBlock::try_new(
                                        Some(watching_path.clone()),
                                        watched_main.clone(),
                                        watched_sdl.clone(),
                                    ) {
                                        Ok(eb) => {
                                            tracing::info!("updating rhai execution engine");
                                            watched_block.store(Arc::new(eb))
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "could not create new rhai execution engine: {}",
                                                e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => tracing::error!("rhai watching event error: {:?}", e),
                    }
                },
                config,
            )
            .unwrap_or_else(|_| panic!("could not create watch on: {watched_path:?}"));
            watcher
                .watch(&watched_path, RecursiveMode::Recursive)
                .unwrap_or_else(|_| panic!("could not watch: {watched_path:?}"));
            // Park the thread until this Rhai instance is dropped (see Drop impl)
            // We may actually unpark() before this code executes or exit from park() spuriously.
            // Use the watching_flag to control a loop which waits from the flag to be updated
            // from Drop.
            while !watching_flag.load(Ordering::Acquire) {
                std::thread::park();
            }
        });

        Ok(Self {
            block,
            park_flag,
            watcher_handle: Some(watcher_handle),
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
            self.block.load().scope.clone(),
        ) {
            tracing::error!("service callback failed: {error}");
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
            self.block.load().scope.clone(),
        ) {
            tracing::error!("service callback failed: {error}");
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
            self.block.load().scope.clone(),
        ) {
            tracing::error!("service callback failed: {error}");
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
            self.block.load().scope.clone(),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }
}

impl Drop for Rhai {
    fn drop(&mut self) {
        if let Some(wh) = self.watcher_handle.take() {
            self.park_flag.store(true, Ordering::Release);
            wh.thread().unpark();
            wh.join().expect("rhai file watcher thread terminating");
        }
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
                        tracing::error!("map_request callback failed: {error_details:#?}");
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return $base::request_failure(request_opt.unwrap().context, error_details);
                    }
                    let mut guard = shared_request.lock().unwrap();
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
                        tracing::error!("map_request callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return $base::request_failure(request_opt.unwrap().context, error_details);
                    }

                    let request_opt = shared_request.lock().unwrap().take();

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

                                let request_opt = shared_request.lock().unwrap().take();
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
                        tracing::error!("map_response callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        return $base::response_failure(
                            response_opt.unwrap().context,
                            error_details,
                        );
                    }
                    let mut guard = shared_response.lock().unwrap();
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
                        tracing::error!("map_response callback failed: {error}");
                        let error_details = process_error(error);
                        let response_opt = shared_response.lock().unwrap().take();
                        return Ok($base::response_failure(
                            response_opt.unwrap().context,
                            error_details
                        ));
                    }

                    let response_opt = shared_response.lock().unwrap().take();

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

                            let response_opt = shared_response.lock().unwrap().take();
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
                    let (first, rest) = stream.into_future().await;

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
                        tracing::error!("map_response callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        return Ok($base::response_failure(
                            response_opt.unwrap().context,
                            error_details
                        ));
                    }

                    let mut guard = shared_response.lock().unwrap();
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
                                tracing::error!("map_response callback failed: {error}");
                                let error_details = process_error(error);
                                let mut guard = shared_response.lock().unwrap();
                                let response_opt = guard.take();
                                let $base::DeferredResponse { mut response, .. } = response_opt.unwrap();
                                let error = Error {
                                    message: error_details.message.unwrap_or_default(),
                                    ..Default::default()
                                };
                                response.errors = vec![error];
                                return Some(response);
                            }

                            let mut guard = shared_response.lock().unwrap();
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
        }
    }

    fn map_response(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
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
        if self.line.is_none() || self.pos.is_none() {
            write!(f, "none")
        } else {
            write!(
                f,
                "line {}, position {}",
                self.line.expect("checked above;qed"),
                self.pos.expect("checked above;qed")
            )
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

    // We only want to process errors raised in functions
    if let EvalAltResult::ErrorInFunctionCall(..) = &*error {
        let inner_error = error.unwrap_inner();
        // We only want to process runtime errors raised in functions
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
        let mut guard = rhai_service.scope.lock().unwrap();
        rhai_service
            .engine
            .call_fn(&mut guard, &rhai_service.ast, callback.fn_name(), args)
    }
}

register_plugin!("apollo", "rhai", Rhai);

#[cfg(test)]
mod tests;
