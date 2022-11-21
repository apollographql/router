//! Customization via Rhai.

use std::collections::HashMap;
use std::fmt;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

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
use once_cell::sync::Lazy;
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
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::external::Externalizable;
use crate::external::PipelineStep;
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

mod macros;

static SDL: Lazy<RwLock<Arc<String>>> = Lazy::new(|| RwLock::new(Arc::new("".to_string())));

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

    #[rhai_fn(name = "map_request")]
    pub(crate) fn map_async_request(rhai_service: &mut RhaiService, callback: FnPtr, url: String) {
        rhai_service
            .service
            .map_async_request(rhai_service.clone(), callback, url)
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

        // Update the global SDL which we'll use as part of our external call interface
        let mut guard = SDL.write().expect("acquiring SDL write lock");
        *guard = sdl;
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

impl ServiceStep {
    fn map_request(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Supergraph(service) => {
                macros::gen_map_deferred_request!(
                    SupergraphRequest,
                    SupergraphResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Execution(service) => {
                macros::gen_map_deferred_request!(
                    ExecutionRequest,
                    ExecutionResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Subgraph(service) => {
                macros::gen_map_request!(subgraph, service, rhai_service, callback);
            }
        }
    }

    fn map_async_request(&mut self, rhai_service: RhaiService, callback: FnPtr, url: String) {
        match self {
            ServiceStep::Supergraph(service) => {
                macros::gen_map_deferred_async_request!(
                    SupergraphRequest,
                    SupergraphResponse,
                    service,
                    rhai_service,
                    callback,
                    url,
                    SupergraphRequest
                );
            }
            ServiceStep::Execution(service) => {
                macros::gen_map_deferred_async_request!(
                    ExecutionRequest,
                    ExecutionResponse,
                    service,
                    rhai_service,
                    callback,
                    url,
                    ExecutionRequest
                );
            }
            ServiceStep::Subgraph(service) => {
                macros::gen_map_async_request!(
                    subgraph,
                    service,
                    rhai_service,
                    callback,
                    url,
                    SubgraphRequest
                );
            }
        }
    }

    fn map_response(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Supergraph(service) => {
                macros::gen_map_deferred_response!(
                    SupergraphResponse,
                    RhaiSupergraphResponse,
                    RhaiSupergraphDeferredResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Execution(service) => {
                macros::gen_map_deferred_response!(
                    ExecutionResponse,
                    RhaiExecutionResponse,
                    RhaiExecutionDeferredResponse,
                    service,
                    rhai_service,
                    callback
                );
            }
            ServiceStep::Subgraph(service) => {
                macros::gen_map_response!(subgraph, service, rhai_service, callback);
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
        macros::register_rhai_interface!(engine, supergraph, execution, subgraph);

        engine
    }

    fn ast_has_function(&self, name: &str) -> bool {
        self.ast.iter_fn_def().any(|fn_def| fn_def.name == name)
    }
}

/// Convert a HeaderMap into a HashMap
fn externalize_header_map(
    input: &HeaderMap<HeaderValue>,
) -> Result<HashMap<String, Vec<String>>, Box<EvalAltResult>> {
    let mut output = HashMap::new();
    for (k, v) in input {
        let k = k.as_str().to_owned();
        let v = String::from_utf8(v.as_bytes().to_vec()).map_err(|e| e.to_string())?;
        output.entry(k).or_insert_with(Vec::new).push(v)
    }
    Ok(output)
}

/// Convert a HashMap into a HeaderMap
fn internalize_header_map(
    input: HashMap<String, Vec<String>>,
) -> Result<HeaderMap<HeaderValue>, Box<EvalAltResult>> {
    let mut output = HeaderMap::new();
    for (k, values) in input {
        for v in values {
            let key = HeaderName::from_str(k.as_ref()).map_err(|e| e.to_string())?;
            let value = HeaderValue::from_str(v.as_ref()).map_err(|e| e.to_string())?;
            output.append(key, value);
        }
    }
    Ok(output)
}

async fn call_external<T>(
    url: String,
    stage: PipelineStep,
    headers: &HeaderMap<HeaderValue>,
    payload: T,
    context: Context,
    sdl: String,
) -> Result<Externalizable<T>, Box<EvalAltResult>>
where
    T: fmt::Debug + DeserializeOwned + Serialize + Send + Sync + 'static,
{
    let converted_headers = externalize_header_map(headers)?;
    let target = Externalizable::new(stage, converted_headers, payload, context, sdl);
    target.call(&url).await.map_err(|e: BoxError| {
        Box::new(EvalAltResult::ErrorSystem(
            "failed to call external component".to_string(),
            e,
        ))
    })
}

register_plugin!("apollo", "rhai", Rhai);

#[cfg(test)]
mod tests;
