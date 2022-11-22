//! Customization via Rhai.

use std::fmt;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;

use futures::future::ready;
use futures::stream::once;
use futures::StreamExt;
use http::header::HeaderName;
use http::header::HeaderValue;
use http::header::InvalidHeaderName;
use http::uri::Authority;
use http::uri::Parts;
use http::uri::PathAndQuery;
use http::HeaderMap;
use http::StatusCode;
use http::Uri;
use opentelemetry::trace::SpanKind;
use rhai::module_resolvers::FileModuleResolver;
use rhai::plugin::*;
use rhai::serde::from_dynamic;
use rhai::serde::to_dynamic;
use rhai::Dynamic;
use rhai::Engine;
use rhai::EvalAltResult;
use rhai::FnPtr;
use rhai::FuncArgs;
use rhai::Instant;
use rhai::Map;
use rhai::Scope;
use rhai::Shared;
use rhai::AST;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::graphql::Request;
use crate::graphql::Response;
use crate::http_ext;
use crate::json_ext::Object;
use crate::json_ext::Value;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::tracer::TraceId;
use crate::Context;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::SupergraphRequest;
use crate::SupergraphResponse;

trait OptionDance<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;

    fn replace(&self, f: impl FnOnce(T) -> T);

    fn take_unwrap(self) -> T;
}

type SharedMut<T> = rhai::Shared<Mutex<Option<T>>>;

impl<T> OptionDance<T> for SharedMut<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock().expect("poisoned mutex");
        f(guard.as_mut().expect("re-entrant option dance"))
    }

    fn replace(&self, f: impl FnOnce(T) -> T) {
        let mut guard = self.lock().expect("poisoned mutex");
        *guard = Some(f(guard.take().expect("re-entrant option dance")))
    }

    fn take_unwrap(self) -> T {
        match Arc::try_unwrap(self) {
            Ok(mutex) => mutex.into_inner().expect("poisoned mutex"),

            // TODO: Should we assume the Arc refcount is 1
            // and use `try_unwrap().expect("shared ownership")` instead of this fallback ?
            Err(arc) => arc.lock().expect("poisoned mutex").take(),
        }
        .expect("re-entrant option dance")
    }
}

mod supergraph {
    pub(crate) use crate::services::supergraph::*;
    pub(crate) type Response = super::RhaiSupergraphResponse;
    pub(crate) type DeferredResponse = super::RhaiSupergraphDeferredResponse;
}

mod execution {
    pub(crate) use crate::services::execution::*;
    pub(crate) type Response = super::RhaiExecutionResponse;
    pub(crate) type DeferredResponse = super::RhaiExecutionDeferredResponse;
}

mod subgraph {
    pub(crate) use crate::services::subgraph::*;
}

#[export_module]
mod router_plugin_mod {
    // It would be nice to generate get_originating_headers and
    // set_originating_headers for all response types.
    // However, variations in the composition
    // of <Type>Response means this isn't currently possible.
    // We could revisit this later if these structures are re-shaped.

    // The next group of functions are specifically for interacting
    // with the subgraph_request on a SubgraphRequest.
    #[rhai_fn(get = "subgraph", pure, return_raw)]
    pub(crate) fn get_subgraph(
        obj: &mut SharedMut<subgraph::Request>,
    ) -> Result<http_ext::Request<Request>, Box<EvalAltResult>> {
        Ok(obj.with_mut(|request| (&request.subgraph_request).into()))
    }

    #[rhai_fn(set = "subgraph", return_raw)]
    pub(crate) fn set_subgraph(
        obj: &mut SharedMut<subgraph::Request>,
        sub: http_ext::Request<Request>,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|request| {
            request.subgraph_request = sub.inner;
            Ok(())
        })
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_subgraph_headers(
        obj: &mut http_ext::Request<Request>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.headers().clone())
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_subgraph_headers(
        obj: &mut http_ext::Request<Request>,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        *obj.headers_mut() = headers;
        Ok(())
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_subgraph_body(
        obj: &mut http_ext::Request<Request>,
    ) -> Result<Request, Box<EvalAltResult>> {
        Ok(obj.body().clone())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_subgraph_body(
        obj: &mut http_ext::Request<Request>,
        body: Request,
    ) -> Result<(), Box<EvalAltResult>> {
        *obj.body_mut() = body;
        Ok(())
    }

    #[rhai_fn(get = "uri", pure, return_raw)]
    pub(crate) fn get_subgraph_uri(
        obj: &mut http_ext::Request<Request>,
    ) -> Result<Uri, Box<EvalAltResult>> {
        Ok(obj.uri().clone())
    }

    #[rhai_fn(set = "uri", return_raw)]
    pub(crate) fn set_subgraph_uri(
        obj: &mut http_ext::Request<Request>,
        uri: Uri,
    ) -> Result<(), Box<EvalAltResult>> {
        *obj.uri_mut() = uri;
        Ok(())
    }
    // End of SubgraphRequest specific section

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_supergraph_response(
        obj: &mut SharedMut<supergraph::Response>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.headers().clone()))
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_router_deferred_response(
        _obj: &mut SharedMut<supergraph::DeferredResponse>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Err("cannot access headers on a deferred response".into())
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_execution_response(
        obj: &mut SharedMut<execution::Response>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.headers().clone()))
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_execution_deferred_response(
        _obj: &mut SharedMut<execution::DeferredResponse>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Err("cannot access headers on a deferred response".into())
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_subgraph_response(
        obj: &mut SharedMut<subgraph::Response>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.headers().clone()))
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_originating_body_supergraph_response(
        obj: &mut SharedMut<supergraph::Response>,
    ) -> Result<Response, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.body().clone()))
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_originating_body_execution_response(
        obj: &mut SharedMut<execution::Response>,
    ) -> Result<Response, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.body().clone()))
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_originating_body_subgraph_response(
        obj: &mut SharedMut<subgraph::Response>,
    ) -> Result<Response, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.body().clone()))
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_originating_body_router_deferred_response(
        obj: &mut SharedMut<supergraph::DeferredResponse>,
    ) -> Result<Response, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.clone()))
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_originating_body_execution_deferred_response(
        obj: &mut SharedMut<execution::DeferredResponse>,
    ) -> Result<Response, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.clone()))
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_originating_headers_supergraph_response(
        obj: &mut SharedMut<supergraph::Response>,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.headers_mut() = headers);
        Ok(())
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_originating_headers_router_deferred_response(
        _obj: &mut SharedMut<supergraph::DeferredResponse>,
        _headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        Err("cannot access headers on a deferred response".into())
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_originating_headers_execution_response(
        obj: &mut SharedMut<execution::Response>,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.headers_mut() = headers);
        Ok(())
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_originating_headers_execution_deferred_response(
        _obj: &mut SharedMut<execution::DeferredResponse>,
        _headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        Err("cannot access headers on a deferred response".into())
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_originating_headers_subgraph_response(
        obj: &mut SharedMut<subgraph::Response>,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.headers_mut() = headers);
        Ok(())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_originating_body_supergraph_response(
        obj: &mut SharedMut<supergraph::Response>,
        body: Response,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.body_mut() = body);
        Ok(())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_originating_body_execution_response(
        obj: &mut SharedMut<execution::Response>,
        body: Response,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.body_mut() = body);
        Ok(())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_originating_body_subraph_response(
        obj: &mut SharedMut<subgraph::Response>,
        body: Response,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.body_mut() = body);
        Ok(())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_originating_body_router_deferred_response(
        obj: &mut SharedMut<supergraph::DeferredResponse>,
        body: Response,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| response.response = body);
        Ok(())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_originating_body_execution_deferred_response(
        obj: &mut SharedMut<execution::DeferredResponse>,
        body: Response,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| response.response = body);
        Ok(())
    }

    pub(crate) fn map_request(rhai_service: &mut RhaiService, callback: FnPtr) {
        rhai_service
            .service
            .map_request(rhai_service.clone(), callback)
    }

    pub(crate) fn map_response(rhai_service: &mut RhaiService, callback: FnPtr) {
        rhai_service
            .service
            .map_response(rhai_service.clone(), callback)
    }
}

/// Plugin which implements Rhai functionality
#[derive(Default, Clone)]
pub(crate) struct Rhai {
    ast: AST,
    engine: Arc<Engine>,
    scope: Arc<Mutex<Scope<'static>>>,
}

/// Configuration for the Rhai Plugin
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conf {
    scripts: Option<PathBuf>,
    main: Option<String>,
}

#[async_trait::async_trait]
impl Plugin for Rhai {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let scripts_path = match init.config.scripts {
            Some(path) => path,
            None => "./rhai".into(),
        };

        let main_file = match init.config.main {
            Some(main) => main,
            None => "main.rhai".to_string(),
        };

        let main = scripts_path.join(&main_file);
        let sdl = init.supergraph_sdl.clone();
        let engine = Arc::new(Rhai::new_rhai_engine(Some(scripts_path)));
        let ast = engine.compile_file(main)?;
        let mut scope = Scope::new();
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
            self.scope.clone(),
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
            self.scope.clone(),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceStep {
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
                        "rhai plugin",
                        "rhai service" = stringify!($base::Request),
                        "otel.kind" = %SpanKind::Internal
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint(move |request: $base::Request| {
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> Result<ControlFlow<$base::Response, $base::Request>, BoxError>
                    {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result: Result<Dynamic, Box<EvalAltResult>> = if $callback.is_curried() {
                        $callback
                            .call(
                                &$rhai_service.engine,
                                &$rhai_service.ast,
                                (shared_request.clone(),),
                            )
                    } else {
                        let mut guard = $rhai_service.scope.lock().unwrap();
                        $rhai_service
                            .engine
                            .call_fn(
                                &mut guard,
                                &$rhai_service.ast,
                                $callback.fn_name(),
                                (shared_request.clone(),),
                            )
                    };
                    if let Err(error) = result {
                        let error_details = process_error(error);
                        tracing::error!("map_request callback failed: {error_details}");
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return failure_message(
                            request_opt.unwrap().context,
                            error_details,
                        );
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
macro_rules! gen_map_deferred_request {
    ($request: ident, $response: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            fn rhai_service_span() -> impl Fn(&$request) -> tracing::Span + Clone {
                move |_request: &$request| {
                    tracing::info_span!(
                        "rhai plugin",
                        "rhai service" = stringify!($request),
                        "otel.kind" = %SpanKind::Internal
                    )
                }
            }
            ServiceBuilder::new()
                .instrument(rhai_service_span())
                .checkpoint(move |request: $request| {
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> Result<ControlFlow<$response, $request>, BoxError> {
                        let res = $response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result = execute(&$rhai_service, &$callback, (shared_request.clone(),));

                    if let Err(error) = result {
                        tracing::error!("map_request callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return failure_message(
                            request_opt.unwrap().context,
                            error_details
                        );
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
macro_rules! gen_map_response {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            service
                .map_response(move |response: $base::Response| {
                    // Let's define a local function to build an error response
                    // XXX: This isn't ideal. We already have a response, so ideally we'd
                    // like to append this error into the existing response. However,
                    // the significantly different treatment of errors in different
                    // response types makes this extremely painful. This needs to be
                    // re-visited at some point post GA.
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> $base::Response {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()
                            .expect("can't fail to build our error message");
                        res
                    }
                    let shared_response = Shared::new(Mutex::new(Some(response)));
                    let result: Result<Dynamic, Box<EvalAltResult>> = if $callback.is_curried() {
                        $callback.call(
                            &$rhai_service.engine,
                            &$rhai_service.ast,
                            (shared_response.clone(),),
                        )
                    } else {
                        let mut guard = $rhai_service.scope.lock().unwrap();
                        $rhai_service.engine.call_fn(
                            &mut guard,
                            &$rhai_service.ast,
                            $callback.fn_name(),
                            (shared_response.clone(),),
                        )
                    };
                    if let Err(error) = result {
                        tracing::error!("map_response callback failed: {error}");
                        let error_details = process_error(error);
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        return failure_message(response_opt.unwrap().context, error_details);
                    }
                    let mut guard = shared_response.lock().unwrap();
                    let response_opt = guard.take();
                    response_opt.unwrap()
                })
                .boxed()
        })
    };
}

macro_rules! gen_map_deferred_response {
    ($response: ident, $rhai_response: ident, $rhai_deferred_response: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            BoxService::new(service.and_then(
                |mapped_response: $response| async move {
                    // Let's define a local function to build an error response
                    // XXX: This isn't ideal. We already have a response, so ideally we'd
                    // like to append this error into the existing response. However,
                    // the significantly different treatment of errors in different
                    // response types makes this extremely painful. This needs to be
                    // re-visited at some point post GA.
                    fn failure_message(
                        context: Context,
                        error_details: ErrorDetails,
                    ) -> $response {
                        let res = $response::error_builder()
                            .errors(vec![Error {
                                message: error_details.message,
                                ..Default::default()
                            }])
                            .status_code(error_details.status)
                            .context(context)
                            .build()
                            .expect("can't fail to build our error message");
                        res
                    }

                    // we split the response stream into headers+first response, then a stream of deferred responses
                    // for which we will implement mapping later
                    let $response { response, context } = mapped_response;
                    let (parts, stream) = response.into_parts();
                    let (first, rest) = stream.into_future().await;

                    if first.is_none() {
                        let error_details = ErrorDetails {
                            status: StatusCode::INTERNAL_SERVER_ERROR,
                            message: "rhai execution error: empty response".to_string(),
                            position: None
                        };
                        return Ok(failure_message(
                            context,
                            error_details
                        ));
                    }

                    let response = $rhai_response {
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
                        return Ok(failure_message(
                            response_opt.unwrap().context,
                            error_details
                        ));
                    }

                    let mut guard = shared_response.lock().unwrap();
                    let response_opt = guard.take();
                    let $rhai_response { context, response } =
                        response_opt.unwrap();
                    let (parts, body) = http::Response::from(response).into_parts();

                    let ctx = context.clone();

                    let mapped_stream = rest.filter_map(move |deferred_response| {
                        let rhai_service = $rhai_service.clone();
                        let context = context.clone();
                        let callback = $callback.clone();
                        async move {
                            let response = $rhai_deferred_response {
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
                                return None;
                            }

                            let mut guard = shared_response.lock().unwrap();
                            let response_opt = guard.take();
                            let $rhai_deferred_response { response, .. } =
                                response_opt.unwrap();
                            Some(response)
                        }
                    });

                    let response = http::Response::from_parts(
                        parts,
                        once(ready(body)).chain(mapped_stream).boxed(),
                    )
                    .into();
                    Ok($response {
                        context: ctx,
                        response,
                    })
                },
            ))
        })
    };
}

#[derive(Default)]
pub(crate) struct RhaiExecutionResponse {
    context: Context,
    response: http_ext::Response<Response>,
}

#[derive(Default)]
pub(crate) struct RhaiExecutionDeferredResponse {
    context: Context,
    response: Response,
}

#[derive(Default)]
pub(crate) struct RhaiSupergraphResponse {
    context: Context,
    response: http_ext::Response<Response>,
}

#[derive(Default)]
pub(crate) struct RhaiSupergraphDeferredResponse {
    context: Context,
    response: Response,
}

macro_rules! if_subgraph {
    ( subgraph => $subgraph: block else $not_subgraph: block ) => {
        $subgraph
    };
    ( $base: ident => $subgraph: block else $not_subgraph: block ) => {
        $not_subgraph
    };
}

macro_rules! register_rhai_interface {
    ($engine: ident, $($base: ident), *) => {
        $(
            // Context stuff
            $engine.register_get(
                "context",
                |obj: &mut SharedMut<$base::Request>| -> Result<Context, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.context.clone()))
                }
            )
            .register_get(
                "context",
                |obj: &mut SharedMut<$base::Response>| -> Result<Context, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|response| response.context.clone()))
                }
            );

            $engine.register_set(
                "context",
                |obj: &mut SharedMut<$base::Request>, context: Context| {
                    obj.with_mut(|request| request.context = context);
                    Ok(())
                }
            )
            .register_set(
                "context",
                |obj: &mut SharedMut<$base::Response>, context: Context| {
                    obj.with_mut(|response| response.context = context);
                    Ok(())
                }
            );

            // Originating Request
            $engine.register_get(
                "headers",
                |obj: &mut SharedMut<$base::Request>| -> Result<HeaderMap, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.supergraph_request.headers().clone()))
                }
            );

            $engine.register_set(
                "headers",
                |obj: &mut SharedMut<$base::Request>, headers: HeaderMap| {
                    if_subgraph! {
                        $base => {
                            let _unused = (obj, headers);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.supergraph_request.headers_mut() = headers);
                            Ok(())
                        }
                    }
                }
            );

            $engine.register_get(
                "body",
                |obj: &mut SharedMut<$base::Request>| -> Result<Request, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.supergraph_request.body().clone()))
                }
            );

            $engine.register_set(
                "body",
                |obj: &mut SharedMut<$base::Request>, body: Request| {
                    if_subgraph! {
                        $base => {
                            let _unused = (obj, body);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.supergraph_request.body_mut() = body);
                            Ok(())
                        }
                    }
                }
            );

            $engine.register_get(
                "uri",
                |obj: &mut SharedMut<$base::Request>| -> Result<Uri, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|request| request.supergraph_request.uri().clone()))
                }
            );

            $engine.register_set(
                "uri",
                |obj: &mut SharedMut<$base::Request>, uri: Uri| {
                    if_subgraph! {
                        $base => {
                            let _unused = (obj, uri);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.supergraph_request.uri_mut() = uri);
                            Ok(())
                        }
                    }
                }
            );
        )*
    };
}

impl ServiceStep {
    fn map_request(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Supergraph(service) => {
                gen_map_deferred_request!(
                    SupergraphRequest,
                    SupergraphResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Execution(service) => {
                gen_map_deferred_request!(
                    ExecutionRequest,
                    ExecutionResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Subgraph(service) => {
                gen_map_request!(subgraph, service, rhai_service, callback);
            }
        }
    }

    fn map_response(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Supergraph(service) => {
                gen_map_deferred_response!(
                    SupergraphResponse,
                    RhaiSupergraphResponse,
                    RhaiSupergraphDeferredResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Execution(service) => {
                gen_map_deferred_response!(
                    ExecutionResponse,
                    RhaiExecutionResponse,
                    RhaiExecutionDeferredResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Subgraph(service) => {
                gen_map_response!(subgraph, service, rhai_service, callback);
            }
        }
    }
}

struct ErrorDetails {
    status: StatusCode,
    message: String,
    position: Option<Position>,
}

impl fmt::Display for ErrorDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some(pos) => {
                write!(f, "{}: {}({})", self.status, self.message, pos)
            }
            None => {
                write!(f, "{}: {}", self.status, self.message)
            }
        }
    }
}

fn process_error(error: Box<EvalAltResult>) -> ErrorDetails {
    let mut error_details = ErrorDetails {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("rhai execution error: '{}'", error),
        position: None,
    };

    // We only want to process errors raised in functions
    if let EvalAltResult::ErrorInFunctionCall(..) = &*error {
        let inner_error = error.unwrap_inner();
        // We only want to process runtime errors raised in functions
        if let EvalAltResult::ErrorRuntime(obj, pos) = inner_error {
            error_details.position = Some(*pos);
            // If we have a dynamic map, try to process it
            if obj.is_map() {
                // Clone is annoying, but we only have a reference, so...
                let map = obj.clone().cast::<Map>();

                let mut ed_status: Option<StatusCode> = None;
                let mut ed_message: Option<String> = None;

                let status_opt = map.get("status");
                let message_opt = map.get("message");

                // Now we have optional Dynamics
                // Try to process each independently
                if let Some(status_dyn) = status_opt {
                    if let Ok(value) = status_dyn.as_int() {
                        if let Ok(status) = StatusCode::try_from(value as u16) {
                            ed_status = Some(status);
                        }
                    }
                }

                if let Some(message_dyn) = message_opt {
                    let cloned = message_dyn.clone();
                    if let Ok(value) = cloned.into_string() {
                        ed_message = Some(value);
                    }
                }

                if let Some(status) = ed_status {
                    // Decide in future if returning a 200 here is ok.
                    // If it is, we can simply remove this check
                    if status != StatusCode::OK {
                        error_details.status = status;
                    }
                }

                if let Some(message) = ed_message {
                    error_details.message = message;
                }
            }
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

#[derive(Clone, Debug)]
pub(crate) struct RhaiService {
    scope: Arc<Mutex<Scope<'static>>>,
    service: ServiceStep,
    engine: Arc<Engine>,
    ast: AST,
}

impl Rhai {
    fn run_rhai_service(
        &self,
        function_name: &str,
        subgraph: Option<&str>,
        service: ServiceStep,
        scope: Arc<Mutex<Scope<'static>>>,
    ) -> Result<(), String> {
        let rhai_service = RhaiService {
            scope: scope.clone(),
            service,
            engine: self.engine.clone(),
            ast: self.ast.clone(),
        };
        let mut guard = scope.lock().unwrap();
        // Note: We don't use `process_error()` here, because this code executes in the context of
        // the pipeline processing. We can't return an HTTP error, we can only return a boxed
        // service which represents the next stage of the pipeline.
        // We could have an error pipeline which always returns results, but that's a big
        // change and one that requires more thought in the future.
        match subgraph {
            Some(name) => {
                self.engine
                    .call_fn(
                        &mut guard,
                        &self.ast,
                        function_name,
                        (rhai_service, name.to_string()),
                    )
                    .map_err(|err| err.to_string())?;
            }
            None => {
                self.engine
                    .call_fn(&mut guard, &self.ast, function_name, (rhai_service,))
                    .map_err(|err| err.to_string())?;
            }
        }

        Ok(())
    }

    fn new_rhai_engine(path: Option<PathBuf>) -> Engine {
        let mut engine = Engine::new();
        // If we pass in a path, use it to configure our engine
        // with a FileModuleResolver which allows import to work
        // in scripts.
        if let Some(scripts) = path {
            let resolver = FileModuleResolver::new_with_path(scripts);
            engine.set_module_resolver(resolver);
        }

        // The macro call creates a Rhai module from the plugin module.
        let module = exported_module!(router_plugin_mod);

        // Configure our engine for execution
        engine
            .set_max_expr_depths(0, 0)
            .on_print(move |rhai_log| {
                tracing::info!("{}", rhai_log);
            })
            // Register our plugin module
            .register_global_module(module.into())
            // Register types accessible in plugin scripts
            .register_type::<Context>()
            .register_type::<HeaderMap>()
            .register_type::<Option<HeaderName>>()
            .register_type::<HeaderName>()
            .register_type::<HeaderValue>()
            .register_type::<(Option<HeaderName>, HeaderValue)>()
            .register_type::<Request>()
            .register_type::<Object>()
            .register_type::<Response>()
            .register_type::<Value>()
            .register_type::<Error>()
            .register_type::<Uri>()
            .register_type::<TraceId>()
            // Register HeaderMap as an iterator so we can loop over contents
            .register_iterator::<HeaderMap>()
            // Register a contains function for HeaderMap so that "in" works
            .register_fn("contains", |x: &mut HeaderMap, key: &str| -> bool {
                match HeaderName::from_str(key) {
                    Ok(hn) => x.contains_key(hn),
                    Err(_e) => false,
                }
            })
            // Register a contains function for Context so that "in" works
            .register_fn("contains", |x: &mut Context, key: &str| -> bool {
                x.get(key).map_or(false, |v: Option<Dynamic>| v.is_some())
            })
            // Register urlencode/decode functions
            .register_fn("urlencode", |x: &mut ImmutableString| -> String {
                urlencoding::encode(x).into_owned()
            })
            .register_fn(
                "urldecode",
                |x: &mut ImmutableString| -> Result<String, Box<EvalAltResult>> {
                    Ok(urlencoding::decode(x)
                        .map_err(|e| e.to_string())?
                        .into_owned())
                },
            )
            .register_fn(
                "headers_are_available",
                |_: &mut SharedMut<supergraph::Response>| -> bool { true },
            )
            .register_fn(
                "headers_are_available",
                |_: &mut SharedMut<supergraph::DeferredResponse>| -> bool { false },
            )
            .register_fn(
                "headers_are_available",
                |_: &mut SharedMut<execution::Response>| -> bool { true },
            )
            .register_fn(
                "headers_are_available",
                |_: &mut SharedMut<execution::DeferredResponse>| -> bool { false },
            )
            // Register a HeaderMap indexer so we can get/set headers
            .register_indexer_get(
                |x: &mut HeaderMap, key: &str| -> Result<String, Box<EvalAltResult>> {
                    let search_name =
                        HeaderName::from_str(key).map_err(|e: InvalidHeaderName| e.to_string())?;
                    Ok(x.get(search_name)
                        .ok_or("")?
                        .to_str()
                        .map_err(|e| e.to_string())?
                        .to_string())
                },
            )
            .register_indexer_set(|x: &mut HeaderMap, key: &str, value: &str| {
                x.insert(
                    HeaderName::from_str(key).map_err(|e| e.to_string())?,
                    HeaderValue::from_str(value).map_err(|e| e.to_string())?,
                );
                Ok(())
            })
            // Register a Context indexer so we can get/set context
            .register_indexer_get(
                |x: &mut Context, key: &str| -> Result<Dynamic, Box<EvalAltResult>> {
                    x.get(key)
                        .map(|v: Option<Dynamic>| v.unwrap_or(Dynamic::UNIT))
                        .map_err(|e: BoxError| e.to_string().into())
                },
            )
            .register_indexer_set(|x: &mut Context, key: &str, value: Dynamic| {
                x.insert(key, value)
                    .map(|v: Option<Dynamic>| v.unwrap_or(Dynamic::UNIT))
                    .map_err(|e: BoxError| e.to_string())?;
                Ok(())
            })
            // Register Context.upsert()
            .register_fn(
                "upsert",
                |context: NativeCallContext,
                 x: &mut Context,
                 key: &str,
                 callback: FnPtr|
                 -> Result<(), Box<EvalAltResult>> {
                    x.upsert(key, |v: Dynamic| -> Dynamic {
                        // Note: Context::upsert() does not allow the callback to fail, although it
                        // can. If call_within_context() fails, return the original provided
                        // value.
                        callback
                            .call_within_context(&context, (v.clone(),))
                            .unwrap_or(v)
                    })
                    .map_err(|e: BoxError| e.to_string().into())
                },
            )
            // Register get for Header Name/Value from a tuple pair
            .register_get("name", |x: &mut (Option<HeaderName>, HeaderValue)| {
                x.0.clone()
            })
            .register_get("value", |x: &mut (Option<HeaderName>, HeaderValue)| {
                x.1.clone()
            })
            // Request.query
            .register_get("query", |x: &mut Request| {
                x.query.clone().map_or(Dynamic::UNIT, Dynamic::from)
            })
            .register_set("query", |x: &mut Request, value: &str| {
                x.query = Some(value.to_string());
            })
            // Request.operation_name
            .register_get("operation_name", |x: &mut Request| {
                x.operation_name
                    .clone()
                    .map_or(Dynamic::UNIT, Dynamic::from)
            })
            .register_set("operation_name", |x: &mut Request, value: &str| {
                x.operation_name = Some(value.to_string());
            })
            // Request.variables
            .register_get("variables", |x: &mut Request| {
                to_dynamic(x.variables.clone())
            })
            .register_set("variables", |x: &mut Request, om: Map| {
                x.variables = from_dynamic(&om.into())?;
                Ok(())
            })
            // Request.extensions
            .register_get("extensions", |x: &mut Request| {
                to_dynamic(x.extensions.clone())
            })
            .register_set("extensions", |x: &mut Request, om: Map| {
                x.extensions = from_dynamic(&om.into())?;
                Ok(())
            })
            // Request.uri.path
            .register_get("path", |x: &mut Uri| to_dynamic(x.path()))
            .register_set("path", |x: &mut Uri, value: &str| {
                // Because there is no simple way to update parts on an existing
                // Uri (no parts_mut()), then we need to create a new Uri from our
                // existing parts, preserving any query, and update our existing
                // Uri.
                let mut parts: Parts = x.clone().into_parts();
                parts.path_and_query = match parts
                    .path_and_query
                    .ok_or("path and query are missing")?
                    .query()
                {
                    Some(query) => Some(
                        PathAndQuery::from_maybe_shared(format!("{}?{}", value, query))
                            .map_err(|e| e.to_string())?,
                    ),
                    None => Some(PathAndQuery::from_str(value).map_err(|e| e.to_string())?),
                };
                *x = Uri::from_parts(parts).map_err(|e| e.to_string())?;
                Ok(())
            })
            // Request.uri.host
            .register_get("host", |x: &mut Uri| to_dynamic(x.host()))
            .register_set("host", |x: &mut Uri, value: &str| {
                // Because there is no simple way to update parts on an existing
                // Uri (no parts_mut()), then we need to create a new Uri from our
                // existing parts, preserving any port, and update our existing
                // Uri.
                let mut parts: Parts = x.clone().into_parts();
                let new_authority = match parts.authority {
                    Some(old_authority) => {
                        if let Some(port) = old_authority.port() {
                            Authority::from_maybe_shared(format!("{}:{}", value, port))
                                .map_err(|e| e.to_string())?
                        } else {
                            Authority::from_str(value).map_err(|e| e.to_string())?
                        }
                    }
                    None => Authority::from_str(value).map_err(|e| e.to_string())?,
                };
                parts.authority = Some(new_authority);
                *x = Uri::from_parts(parts).map_err(|e| e.to_string())?;
                Ok(())
            })
            // Response.label
            .register_get("label", |x: &mut Response| {
                x.label.clone().map_or(Dynamic::UNIT, Dynamic::from)
            })
            .register_set("label", |x: &mut Response, value: &str| {
                x.label = Some(value.to_string());
            })
            // Response.data
            .register_get("data", |x: &mut Response| to_dynamic(x.data.clone()))
            .register_set("data", |x: &mut Response, om: Map| {
                x.data = from_dynamic(&om.into())?;
                Ok(())
            })
            // Response.path (Not Implemented)
            // Response.errors
            .register_get("errors", |x: &mut Response| to_dynamic(x.errors.clone()))
            .register_set("errors", |x: &mut Response, value: Dynamic| {
                x.errors = from_dynamic(&value)?;
                Ok(())
            })
            // Response.extensions
            .register_get("extensions", |x: &mut Response| {
                to_dynamic(x.extensions.clone())
            })
            .register_set("extensions", |x: &mut Response, om: Map| {
                x.extensions = from_dynamic(&om.into())?;
                Ok(())
            })
            // TraceId support
            .register_fn("traceid", || -> Result<TraceId, Box<EvalAltResult>> {
                TraceId::maybe_new().ok_or_else(|| "trace unavailable".into())
            })
            .register_fn("to_string", |id: &mut TraceId| -> String { id.to_string() })
            // Register a series of logging functions
            .register_fn("log_trace", |out: Dynamic| {
                tracing::trace!(%out, "rhai_trace");
            })
            .register_fn("log_debug", |out: Dynamic| {
                tracing::debug!(%out, "rhai_debug");
            })
            .register_fn("log_info", |out: Dynamic| {
                tracing::info!(%out, "rhai_info");
            })
            .register_fn("log_warn", |out: Dynamic| {
                tracing::warn!(%out, "rhai_warn");
            })
            .register_fn("log_error", |out: Dynamic| {
                tracing::error!(%out, "rhai_error");
            })
            // Register a function for printing to stderr
            .register_fn("eprint", |x: &str| {
                eprintln!("{}", x);
            })
            // Default representation in rhai is the "type", so
            // we need to register a to_string function for all our registered
            // types so we can interact meaningfully with them.
            .register_fn("to_string", |x: &mut Context| -> String {
                format!("{:?}", x)
            })
            .register_fn("to_string", |x: &mut Option<HeaderName>| -> String {
                match x {
                    Some(v) => v.to_string(),
                    None => "None".to_string(),
                }
            })
            .register_fn("to_string", |x: &mut HeaderName| -> String {
                x.to_string()
            })
            .register_fn("to_string", |x: &mut HeaderValue| -> String {
                x.to_str().map_or("".to_string(), |v| v.to_string())
            })
            .register_fn("to_string", |x: &mut HeaderMap| -> String {
                let mut msg = String::new();
                for pair in x.iter() {
                    let line = format!(
                        "{}: {}",
                        pair.0,
                        pair.1.to_str().map_or("".to_string(), |v| v.to_string())
                    );
                    msg.push_str(line.as_ref());
                    msg.push('\n');
                }
                msg
            })
            .register_fn(
                "to_string",
                |x: &mut (Option<HeaderName>, HeaderValue)| -> String {
                    format!(
                        "{}: {}",
                        match &x.0 {
                            Some(v) => v.to_string(),
                            None => "None".to_string(),
                        },
                        x.1.to_str().map_or("".to_string(), |v| v.to_string())
                    )
                },
            )
            .register_fn("to_string", |x: &mut Request| -> String {
                format!("{:?}", x)
            })
            .register_fn("to_string", |x: &mut Response| -> String {
                format!("{:?}", x)
            })
            .register_fn("to_string", |x: &mut Error| -> String {
                format!("{:?}", x)
            })
            .register_fn("to_string", |x: &mut Object| -> String {
                format!("{:?}", x)
            })
            .register_fn("to_string", |x: &mut Value| -> String {
                format!("{:?}", x)
            })
            .register_fn("to_string", |x: &mut Uri| -> String { format!("{:?}", x) })
            // Add query plan getter to execution request
            .register_get(
                "query_plan",
                |obj: &mut SharedMut<execution::Request>| -> String {
                    obj.with_mut(|request| {
                        request
                            .query_plan
                            .formatted_query_plan
                            .clone()
                            .unwrap_or_default()
                    })
                },
            )
            // Add context getter/setters for deferred responses
            .register_get(
                "context",
                |obj: &mut SharedMut<supergraph::DeferredResponse>| -> Result<Context, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|response| response.context.clone()))
                },
            )
            .register_set(
                "context",
                |obj: &mut SharedMut<supergraph::DeferredResponse>, context: Context| {
                    obj.with_mut(|response| response.context = context);
                    Ok(())
                },
            )
            .register_get(
                "context",
                |obj: &mut SharedMut<execution::DeferredResponse>| -> Result<Context, Box<EvalAltResult>> {
                    Ok(obj.with_mut(|response| response.context.clone()))
                },
            )
            .register_set(
                "context",
                |obj: &mut SharedMut<execution::DeferredResponse>, context: Context| {
                    obj.with_mut(|response| response.context = context);
                    Ok(())
                },
            );
        // Add common getter/setters for different types
        register_rhai_interface!(engine, supergraph, execution, subgraph);

        engine
    }

    fn ast_has_function(&self, name: &str) -> bool {
        self.ast.iter_fn_def().any(|fn_def| fn_def.name == name)
    }
}

register_plugin!("apollo", "rhai", Rhai);

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use rhai::EvalAltResult;
    use serde_json::Value;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;
    use crate::http_ext;
    use crate::plugin::test::MockExecutionService;
    use crate::plugin::test::MockSupergraphService;
    use crate::plugin::DynPlugin;
    use crate::SubgraphRequest;

    #[tokio::test]
    async fn rhai_plugin_router_service() -> Result<(), BoxError> {
        let mut mock_service = MockSupergraphService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: SupergraphRequest| {
                Ok(SupergraphResponse::fake_builder()
                    .header("x-custom-header", "CUSTOM_VALUE")
                    .context(req.context)
                    .build()
                    .unwrap())
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();
        let mut router_service = dyn_plugin.supergraph_service(BoxService::new(mock_service));
        let context = Context::new();
        context.insert("test", 5i64).unwrap();
        let supergraph_req = SupergraphRequest::fake_builder().context(context).build()?;

        let mut supergraph_resp = router_service.ready().await?.call(supergraph_req).await?;
        assert_eq!(supergraph_resp.response.status(), 200);
        let headers = supergraph_resp.response.headers().clone();
        let context = supergraph_resp.context.clone();
        // Check if it fails
        let resp = supergraph_resp.next_response().await.unwrap();
        if !resp.errors.is_empty() {
            panic!(
                "Contains errors : {}",
                resp.errors
                    .into_iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<String>>()
                    .join("\n")
            );
        }

        assert_eq!(headers.get("coucou").unwrap(), &"hello");
        assert_eq!(headers.get("coming_from_entries").unwrap(), &"value_15");
        assert_eq!(context.get::<_, i64>("test").unwrap().unwrap(), 42i64);
        assert_eq!(
            context.get::<_, String>("addition").unwrap().unwrap(),
            "Here is a new element in the context".to_string()
        );
        Ok(())
    }

    #[tokio::test]
    async fn rhai_plugin_execution_service_error() -> Result<(), BoxError> {
        let mut mock_service = MockExecutionService::new();
        mock_service.expect_clone().return_once(move || {
            let mut mock_service = MockExecutionService::new();
            // The execution_service in test.rhai throws an exception, so we never
            // get a call into the mock service...
            mock_service.expect_call().never();
            mock_service
        });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();
        let mut router_service = dyn_plugin.execution_service(BoxService::new(mock_service));
        let fake_req = http_ext::Request::fake_builder()
            .header("x-custom-header", "CUSTOM_VALUE")
            .body(Request::builder().query(String::new()).build())
            .build()?;
        let context = Context::new();
        context.insert("test", 5i64).unwrap();
        let exec_req = ExecutionRequest::fake_builder()
            .context(context)
            .supergraph_request(fake_req)
            .build();

        let mut exec_resp = router_service
            .ready()
            .await
            .unwrap()
            .call(exec_req)
            .await
            .unwrap();
        assert_eq!(
            exec_resp.response.status(),
            http::StatusCode::INTERNAL_SERVER_ERROR
        );
        // Check if it fails
        let body = exec_resp.next_response().await.unwrap();
        if body.errors.is_empty() {
            panic!(
                "Must contain errors : {}",
                body.errors
                    .into_iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<String>>()
                    .join("\n")
            );
        }

        assert_eq!(
            body.errors.get(0).unwrap().message.as_str(),
            "rhai execution error: 'Runtime error: An error occured (line 30, position 5) in call to function execution_request'"
        );
        Ok(())
    }

    // Some of these tests rely extensively on internal implementation details of the tracing_test crate.
    // These are unstable, so these test may break if the tracing_test crate is updated.
    //
    // This is done to avoid using the public interface of tracing_test which installs a global
    // subscriber which breaks other tests in our stack which also insert a global subscriber.
    // (there can be only one...) which means we cannot test it with #[tokio::test(flavor = "multi_thread")]
    #[test]
    fn it_logs_messages() {
        let env_filter = "apollo_router=trace";
        let mock_writer =
            tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
        let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

        let _guard = tracing::dispatcher::set_default(&subscriber);
        let engine = Rhai::new_rhai_engine(None);
        let input_logs = vec![
            r#"log_trace("trace log")"#,
            r#"log_debug("debug log")"#,
            r#"log_info("info log")"#,
            r#"log_warn("warn log")"#,
            r#"log_error("error log")"#,
        ];
        for log in input_logs {
            engine.eval::<()>(log).expect("it logged a message");
        }
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "trace log"
        ));
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "debug log"
        ));
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "info log"
        ));
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "warn log"
        ));
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "error log"
        ));
    }

    #[test]
    fn it_prints_messages_to_log() {
        let env_filter = "apollo_router=trace";
        let mock_writer =
            tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
        let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

        let _guard = tracing::dispatcher::set_default(&subscriber);
        let engine = Rhai::new_rhai_engine(None);
        engine
            .eval::<()>(r#"print("info log")"#)
            .expect("it logged a message");
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "info log"
        ));
    }

    #[tokio::test]
    async fn it_can_access_sdl_constant() {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();

        // Downcast our generic plugin. We know it must be Rhai
        let it: &dyn std::any::Any = dyn_plugin.as_any();
        let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

        // Get a scope to use for our test
        let scope = rhai_instance.scope.clone();

        let mut guard = scope.lock().unwrap();

        // Call our function to make sure we can access the sdl
        let sdl: String = rhai_instance
            .engine
            .call_fn(&mut guard, &rhai_instance.ast, "get_sdl", ())
            .expect("can get sdl");
        assert_eq!(sdl.as_str(), "");
    }

    #[test]
    fn it_provides_helpful_headermap_errors() {
        let mut engine = Rhai::new_rhai_engine(None);
        engine.register_fn("new_hm", HeaderMap::new);

        let result = engine.eval::<HeaderMap>(
            r#"
    let map = new_hm();
    map["ümlaut"] = "will fail";
    map
"#,
        );
        assert!(result.is_err());
        assert!(matches!(
            *result.unwrap_err(),
            EvalAltResult::ErrorRuntime(..)
        ));
    }

    // There is a lot of repetition in these tests, so I've tried to reduce that with these two
    // macros. The repetition could probably be reduced further, but ...

    macro_rules! gen_request_test {
        ($base: ident, $fn_name: literal) => {
            let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
                .get("apollo.rhai")
                .expect("Plugin not found")
                .create_instance_without_schema(
                    &Value::from_str(
                        r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();

            // Downcast our generic plugin. We know it must be Rhai
            let it: &dyn std::any::Any = dyn_plugin.as_any();
            let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

            // Get a scope to use for our test
            let scope = rhai_instance.scope.clone();

            let mut guard = scope.lock().unwrap();

            // We must wrap our canned request in Arc<Mutex<Option<>>> to keep the rhai runtime
            // happy
            let request = Arc::new(Mutex::new(Some($base::fake_builder().build())));

            // Call our rhai test function. If it return an error, the test failed.
            let result: Result<(), Box<rhai::EvalAltResult>> =
                rhai_instance
                    .engine
                    .call_fn(&mut guard, &rhai_instance.ast, $fn_name, (request,));
            result.expect("test failed");
        };
    }

    macro_rules! gen_response_test {
        ($base: ident, $fn_name: literal) => {
            let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
                .get("apollo.rhai")
                .expect("Plugin not found")
                .create_instance_without_schema(
                    &Value::from_str(
                        r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();

            // Downcast our generic plugin. We know it must be Rhai
            let it: &dyn std::any::Any = dyn_plugin.as_any();
            let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

            // Get a scope to use for our test
            let scope = rhai_instance.scope.clone();

            let mut guard = scope.lock().unwrap();

            // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
            // happy
            let response = Arc::new(Mutex::new(Some($base::default())));

            // Call our rhai test function. If it return an error, the test failed.
            let result: Result<(), Box<rhai::EvalAltResult>> =
                rhai_instance
                    .engine
                    .call_fn(&mut guard, &rhai_instance.ast, $fn_name, (response,));
            result.expect("test failed");
        };
    }

    #[tokio::test]
    async fn it_can_process_supergraph_request() {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        // Downcast our generic plugin. We know it must be Rhai
        let it: &dyn std::any::Any = dyn_plugin.as_any();
        let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

        // Get a scope to use for our test
        let scope = rhai_instance.scope.clone();

        let mut guard = scope.lock().unwrap();

        // We must wrap our canned request in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let request = Arc::new(Mutex::new(Some(
            SupergraphRequest::canned_builder()
                .operation_name("canned")
                .build()
                .expect("build canned supergraph request"),
        )));

        // Call our rhai test function. If it return an error, the test failed.
        let result: Result<(), Box<rhai::EvalAltResult>> = rhai_instance.engine.call_fn(
            &mut guard,
            &rhai_instance.ast,
            "process_supergraph_request",
            (request,),
        );
        result.expect("test failed");
    }

    #[tokio::test]
    async fn it_can_process_execution_request() {
        gen_request_test!(ExecutionRequest, "process_execution_request");
    }

    #[tokio::test]
    async fn it_can_process_subgraph_request() {
        gen_request_test!(SubgraphRequest, "process_subgraph_request");
    }

    #[tokio::test]
    async fn it_can_process_supergraph_response() {
        gen_response_test!(RhaiSupergraphResponse, "process_supergraph_response");
    }

    #[tokio::test]
    async fn it_can_process_supergraph_deferred_response() {
        gen_response_test!(
            RhaiSupergraphDeferredResponse,
            "process_supergraph_response"
        );
    }

    #[tokio::test]
    async fn it_can_process_execution_response() {
        gen_response_test!(RhaiExecutionResponse, "process_execution_response");
    }

    #[tokio::test]
    async fn it_can_process_execution_deferred_response() {
        gen_response_test!(RhaiExecutionDeferredResponse, "process_execution_response");
    }

    #[tokio::test]
    async fn it_can_process_subgraph_response() {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        // Downcast our generic plugin. We know it must be Rhai
        let it: &dyn std::any::Any = dyn_plugin.as_any();
        let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

        // Get a scope to use for our test
        let scope = rhai_instance.scope.clone();

        let mut guard = scope.lock().unwrap();

        // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let response = Arc::new(Mutex::new(Some(subgraph::Response::fake_builder().build())));

        // Call our rhai test function. If it return an error, the test failed.
        let result: Result<(), Box<rhai::EvalAltResult>> = rhai_instance.engine.call_fn(
            &mut guard,
            &rhai_instance.ast,
            "process_subgraph_response",
            (response,),
        );
        result.expect("test failed");
    }

    #[test]
    fn it_can_urlencode_string() {
        let engine = Rhai::new_rhai_engine(None);
        let encoded: String = engine
            .eval(r#"urlencode("This has an ümlaut in it.")"#)
            .expect("can encode string");
        assert_eq!(encoded, "This%20has%20an%20%C3%BCmlaut%20in%20it.");
    }

    #[test]
    fn it_can_urldecode_string() {
        let engine = Rhai::new_rhai_engine(None);
        let decoded: String = engine
            .eval(r#"urldecode("This%20has%20an%20%C3%BCmlaut%20in%20it.")"#)
            .expect("can decode string");
        assert_eq!(decoded, "This has an ümlaut in it.");
    }

    async fn base_process_function(fn_name: &str) -> Result<(), Box<rhai::EvalAltResult>> {
        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance_without_schema(
                &Value::from_str(
                    r#"{"scripts":"tests/fixtures", "main":"request_response_test.rhai"}"#,
                )
                .unwrap(),
            )
            .await
            .unwrap();

        // Downcast our generic plugin. We know it must be Rhai
        let it: &dyn std::any::Any = dyn_plugin.as_any();
        let rhai_instance: &Rhai = it.downcast_ref::<Rhai>().expect("downcast");

        // Get a scope to use for our test
        let scope = rhai_instance.scope.clone();

        let mut guard = scope.lock().unwrap();

        // We must wrap our canned response in Arc<Mutex<Option<>>> to keep the rhai runtime
        // happy
        let response = Arc::new(Mutex::new(Some(subgraph::Response::fake_builder().build())));

        // Call our rhai test function. If it doesn't return an error, the test failed.
        rhai_instance
            .engine
            .call_fn(&mut guard, &rhai_instance.ast, fn_name, (response,))
    }

    #[tokio::test]
    async fn it_can_process_om_subgraph_forbidden() {
        if let Err(error) = base_process_function("process_subgraph_response_om_forbidden").await {
            let processed_error = process_error(error);
            assert_eq!(processed_error.status, StatusCode::FORBIDDEN);
            assert_eq!(processed_error.message, "I have raised a 403");
        } else {
            // Test failed
            panic!("error processed incorrectly");
        }
    }

    #[tokio::test]
    async fn it_can_process_string_subgraph_forbidden() {
        if let Err(error) = base_process_function("process_subgraph_response_string").await {
            let processed_error = process_error(error);
            assert_eq!(processed_error.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(processed_error.message, "rhai execution error: 'Runtime error: I have raised an error (line 124, position 5) in call to function process_subgraph_response_string'");
        } else {
            // Test failed
            panic!("error processed incorrectly");
        }
    }

    #[tokio::test]
    async fn it_cannot_process_ok_subgraph_forbidden() {
        if let Err(error) = base_process_function("process_subgraph_response_om_ok").await {
            let processed_error = process_error(error);
            assert_eq!(processed_error.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(processed_error.message, "I have raised a 200");
        } else {
            // Test failed
            panic!("error processed incorrectly");
        }
    }

    #[tokio::test]
    async fn it_cannot_process_om_subgraph_missing_message() {
        if let Err(error) =
            base_process_function("process_subgraph_response_om_missing_message").await
        {
            let processed_error = process_error(error);
            assert_eq!(processed_error.status, StatusCode::BAD_REQUEST);
            assert_eq!(processed_error.message, "rhai execution error: 'Runtime error: #{\"status\": 400} (line 135, position 5) in call to function process_subgraph_response_om_missing_message'");
        } else {
            // Test failed
            panic!("error processed incorrectly");
        }
    }
}
