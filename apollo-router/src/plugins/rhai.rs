//! Customization via Rhai.

use apollo_router_core::{
    http_compat, register_plugin, Context, ExecutionRequest, ExecutionResponse, Plugin,
    QueryPlannerRequest, QueryPlannerResponse, Request, ResponseBody, RouterRequest,
    RouterResponse, ServiceBuilderExt, SubgraphRequest, SubgraphResponse,
};
use http::header::{HeaderName, HeaderValue, InvalidHeaderName};
use http::{HeaderMap, StatusCode};
use rhai::{plugin::*, Dynamic, Engine, EvalAltResult, FnPtr, Instant, Scope, Shared, AST};
use schemars::JsonSchema;
use serde::Deserialize;
use std::str::FromStr;
use std::{
    ops::ControlFlow,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

pub trait Accessor<Access>: Send {
    fn accessor(&self) -> &Access;

    fn accessor_mut(&mut self) -> &mut Access;
}

trait OptionDance<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;

    fn replace(&self, f: impl FnOnce(T) -> T);

    fn take_unwrap(self) -> T;
}

impl<T> OptionDance<T> for Arc<Mutex<Option<T>>> {
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

#[export_module]
mod router_plugin_mod {
    macro_rules! gen_rhai_interface {
        ($ ($base: ident), +) => {
            #[export_module]
            pub(crate) mod router_generated_mod {
                $(
            paste::paste! {

                pub fn [<get_operation_name_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                ) -> Dynamic {
                    obj.with_mut(get_operation_name)
                }

                pub fn [<get_context_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                ) -> Result<Context, Box<EvalAltResult>> {
                    obj.with_mut(get_context)
                }

                pub fn [<get_context_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                ) -> Result<Context, Box<EvalAltResult>> {
                    obj.with_mut(get_context)
                }

                pub fn [<insert_context_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                    context: Context
                ) -> Result<(), Box<EvalAltResult>> {
                    obj.with_mut(|request| insert_context(request, context))
                }

                pub fn [<insert_context_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                    context: Context
                ) -> Result<(), Box<EvalAltResult>> {
                    obj.with_mut(|response| insert_context(response, context))
                }

                pub fn [<get_originating_headers_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                ) -> Result<HeaderMap, Box<EvalAltResult>> {
                    obj.with_mut(get_originating_headers)
                }

                /* XXX ONLY VALID FOR CERTAIN TYPES
                 *
                pub fn [<get_originating_headers_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                ) -> Result<HeaderMap, Box<EvalAltResult>> {
                    obj.with_mut(get_originating_headers)
                }
                */

                pub fn [<set_originating_headers_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                    headers: HeaderMap
                ) -> Result<(), Box<EvalAltResult>> {
                    obj.with_mut(|request| set_originating_headers(request, headers))
                }

                /* XXX ONLY VALID FOR CERTAIN TYPES
                pub fn [<set_originating_headers_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                    headers: HeaderMap
                ) -> Result<(), Box<EvalAltResult>> {
                    obj.with_mut(|response| set_originating_headers(response, context))
                }
                */
            }
                )*
            }
        };
    }

    #[rhai_fn(get = "sub_headers", return_raw)]
    pub fn get_subgraph_headers(
        obj: &mut SharedSubgraphRequest,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        obj.with_mut(|request| Ok(request.subgraph_request.headers().clone()))
    }

    #[rhai_fn(set = "sub_headers", return_raw)]
    pub fn set_subgraph_headers(
        obj: &mut SharedSubgraphRequest,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|request| {
            *request.subgraph_request.headers_mut() = headers;
            Ok(())
        })
    }

    #[rhai_fn(get = "headers", return_raw)]
    pub fn get_originating_headers_router_response(
        obj: &mut SharedRouterResponse,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        obj.with_mut(get_originating_headers_response_response_body)
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub fn set_originating_headers_router_response(
        obj: &mut SharedRouterResponse,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| set_originating_headers_response_response_body(response, headers))
    }

    // This is a getter for 'RouterRequest::operation_name'.
    fn get_operation_name<T: Accessor<http_compat::Request<Request>>>(obj: &mut T) -> Dynamic {
        obj.accessor()
            .body()
            .operation_name
            .clone()
            .map_or(Dynamic::from(()), Dynamic::from)
    }

    fn get_context<T: Accessor<Context>>(obj: &mut T) -> Result<Context, Box<EvalAltResult>> {
        Ok(obj.accessor().clone())
    }

    fn insert_context<T: Accessor<Context>>(
        obj: &mut T,
        context: Context,
    ) -> Result<(), Box<EvalAltResult>> {
        *obj.accessor_mut() = context;
        Ok(())
    }

    fn get_originating_headers<T: Accessor<http_compat::Request<Request>>>(
        obj: &mut T,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.accessor().headers().clone())
    }

    fn get_originating_headers_response_response_body<
        T: Accessor<http_compat::Response<ResponseBody>>,
    >(
        obj: &mut T,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.accessor().headers().clone())
    }

    fn set_originating_headers<T: Accessor<http_compat::Request<Request>>>(
        obj: &mut T,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        *obj.accessor_mut().headers_mut() = headers;
        Ok(())
    }

    fn set_originating_headers_response_response_body<
        T: Accessor<http_compat::Response<ResponseBody>>,
    >(
        obj: &mut T,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        *obj.accessor_mut().headers_mut() = headers;
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

    gen_rhai_interface!(router, query_planner, execution, subgraph);
}

#[derive(Default, Clone)]
pub struct Rhai {
    ast: AST,
    engine: Arc<Engine>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Conf {
    filename: PathBuf,
}

#[async_trait::async_trait]
impl Plugin for Rhai {
    type Config = Conf;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        let engine = Arc::new(Rhai::new_rhai_engine());
        let ast = engine.compile_file(configuration.filename)?;
        Ok(Self { ast, engine })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "router_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("router_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            ServiceStep::Router(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "query_planner_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("query_planner_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            ServiceStep::QueryPlanner(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "execution_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("execution_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            ServiceStep::Execution(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "subgraph_service";
        if !self.ast_has_function(FUNCTION_NAME_SERVICE) {
            return service;
        }
        tracing::debug!("subgraph_service function found");
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if let Err(error) = self.run_rhai_service(
            FUNCTION_NAME_SERVICE,
            ServiceStep::Subgraph(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceStep {
    Router(SharedRouterService),
    QueryPlanner(SharedQueryPlannerService),
    Execution(SharedExecutionService),
    Subgraph(SharedSubgraphService),
}

macro_rules! accessor_mut_for_shared_types {
    (subgraph) => {
        // XXX CAN'T DO THIS FOR SUBGRAPH
        fn accessor_mut(&mut self) -> &mut http_compat::Request<Request> {
            panic!("cannot mutate originating request on a subgraph");
        }
    };
    ($_base: ident) => {
        fn accessor_mut(&mut self) -> &mut http_compat::Request<Request> {
            &mut self.originating_request
        }
    };
}

macro_rules! gen_shared_types {
    ($($base: ident), +) => {
        $(
        paste::paste! {
            #[allow(dead_code)]
            type [<Shared $base:camel Service>] = Arc<Mutex<Option<BoxService<[<$base:camel Request>], [<$base:camel Response>], BoxError>>>>;

            #[allow(dead_code)]
            type [<Shared $base:camel Request>] = Arc<Mutex<Option<[<$base:camel Request>]>>>;

            #[allow(dead_code)]
            type [<Shared $base:camel Response>] = Arc<Mutex<Option<[<$base:camel Response>]>>>;

            impl Accessor<Context> for [<$base:camel Request >] {

                fn accessor(&self) -> &Context {
                    &self.context
                }

                fn accessor_mut(&mut self) -> &mut Context {
                    &mut self.context
                }
            }

            impl Accessor<Context> for [<$base:camel Response >] {

                fn accessor(&self) -> &Context {
                    &self.context
                }

                fn accessor_mut(&mut self) -> &mut Context {
                    &mut self.context
                }
            }

            impl Accessor<http_compat::Request<Request>> for [<$base:camel Request >] {

                fn accessor(&self) -> &http_compat::Request<Request> {
                    &self.originating_request
                }

                accessor_mut_for_shared_types!($base);
            }
        }
        )*
    };
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        paste::paste! {
            $borrow.replace(|service| {
                ServiceBuilder::new()
                    .checkpoint(move |request: [<$base:camel Request>]| {
                        // Let's define a local function to build an error response
                        fn failure_message(
                            context: Context,
                            msg: String,
                            status: StatusCode,
                        ) -> Result<ControlFlow<[<$base:camel Response>], [<$base:camel Request>]>, BoxError> {
                            let res = [<$base:camel Response>]::error_builder()
                                .errors(vec![apollo_router_core::Error {
                                    message: msg,
                                    ..Default::default()
                                }])
                                .status_code(status)
                                .context(context)
                                .build()?;
                            Ok(ControlFlow::Break(res))
                        }
                        let shared_request = Shared::new(Mutex::new(Some(request)));
                        let mut scope = $rhai_service.scope.clone();
                        let result: Result<Dynamic, String> = $rhai_service.engine
                            .call_fn(&mut scope, &$rhai_service.ast, $callback.fn_name(), (shared_request.clone(),))
                            .map_err(|err| err.to_string());
                        if let Err(error) = result {
                            tracing::error!("map_request callback failed: {error}");
                            let mut guard = shared_request.lock().unwrap();
                            let request_opt = guard.take();
                            return failure_message(request_opt.unwrap().context, format!("rhai execution error: '{}'", error), StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        Ok(ControlFlow::Continue(request_opt.unwrap()))
                    })
                    .service(service)
                    .boxed()
            })
        }
    };
}

macro_rules! gen_map_response {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        paste::paste! {
            $borrow.replace(|service| {
                service
                    .map_response(move |response: [<$base:camel Response>]| {
                        // Let's define a local function to build an error response
                        // XXX: This isn't ideal. We already have a response, so ideally we'd
                        // like to append this error into the existing response. However,
                        // the significantly different treatment of errors in different
                        // response types makes this extremely painful. This needs to be
                        // re-visited at some point post GA.
                        fn failure_message(
                            context: Context,
                            msg: String,
                            status: StatusCode,
                        ) -> [<$base:camel Response>] {
                            let res = [<$base:camel Response>]::error_builder()
                                .errors(vec![apollo_router_core::Error {
                                    message: msg,
                                    ..Default::default()
                                }])
                                .status_code(status)
                                .context(context)
                                .build()
                                .expect("can't fail to build our error message");
                            res
                        }
                        let shared_response = Shared::new(Mutex::new(Some(response)));
                        let mut scope = $rhai_service.scope.clone();
                        let result: Result<Dynamic, String> = $rhai_service.engine
                            .call_fn(&mut scope, &$rhai_service.ast, $callback.fn_name(), (shared_response.clone(),))
                            .map_err(|err| err.to_string());
                        if let Err(error) = result {
                            tracing::error!("map_response callback failed: {error}");
                            let mut guard = shared_response.lock().unwrap();
                            let response_opt = guard.take();
                            return failure_message(response_opt.unwrap().context, format!("rhai execution error: '{}'", error), StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        response_opt.unwrap()
                    })
                    .boxed()
            })
        }
    };
}

// Special case for subgraph, so invoke separately
gen_shared_types!(router, query_planner, execution);
gen_shared_types!(subgraph);

impl Accessor<http_compat::Response<ResponseBody>> for RouterResponse {
    fn accessor(&self) -> &http_compat::Response<ResponseBody> {
        &self.response
    }

    fn accessor_mut(&mut self) -> &mut http_compat::Response<ResponseBody> {
        &mut self.response
    }
}

macro_rules! register_rhai_interface {
    ($engine: ident, $($base: ident), *) => {
        $(
            paste::paste! {
            // Operation name
            $engine.register_get(
                "operation_name",
                router_plugin_mod::router_generated_mod::[<get_operation_name_ $base _request>],
            );

            // Context stuff
            $engine.register_get_result(
                "context",
                router_plugin_mod::router_generated_mod::[<get_context_ $base _request>],
            )
            .register_get_result(
                "context",
                router_plugin_mod::router_generated_mod::[<get_context_ $base _response>],
            );

            $engine.register_set_result(
                "context",
                router_plugin_mod::router_generated_mod::[<insert_context_ $base _request>],
            )
            .register_set_result(
                "context",
                router_plugin_mod::router_generated_mod::[<insert_context_ $base _response>],
            );

            // Originating Request
            $engine.register_get_result(
                "headers",
                router_plugin_mod::router_generated_mod::[<get_originating_headers_ $base _request>],
            );

            $engine.register_set_result(
                "headers",
                router_plugin_mod::router_generated_mod::[<set_originating_headers_ $base _request>],
            );

            }

        )*
    };
}

impl ServiceStep {
    // fn map_request(&mut self, engine: Arc<Engine>, scope: Scope, ast: AST, callback: FnPtr) {
    fn map_request(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Router(service) => {
                gen_map_request!(router, service, rhai_service, callback);
            }
            ServiceStep::QueryPlanner(service) => {
                gen_map_request!(query_planner, service, rhai_service, callback);
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
                gen_map_response!(router, service, rhai_service, callback);
            }
            ServiceStep::QueryPlanner(service) => {
                gen_map_response!(query_planner, service, rhai_service, callback);
            }
            ServiceStep::Execution(service) => {
                gen_map_response!(execution, service, rhai_service, callback);
            }
            ServiceStep::Subgraph(service) => {
                gen_map_response!(subgraph, service, rhai_service, callback);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RhaiService {
    scope: Scope<'static>,
    service: ServiceStep,
    engine: Arc<Engine>,
    ast: AST,
}

impl Rhai {
    fn run_rhai_service(&self, function_name: &str, service: ServiceStep) -> Result<(), String> {
        let mut scope = Scope::new();
        scope.push_constant("apollo_start", Instant::now());
        let rhai_service = RhaiService {
            scope: scope.clone(),
            service,
            engine: self.engine.clone(),
            ast: self.ast.clone(),
        };
        self.engine
            .call_fn(&mut scope, &self.ast, function_name, (rhai_service,))
            .map_err(|err| err.to_string())?;

        Ok(())
    }

    fn new_rhai_engine() -> Engine {
        let mut engine = Engine::new();

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
            // Register HeaderMap as an iterator so we can loop over contents
            .register_iterator::<HeaderMap>()
            // Register a contains function for HeaderMap so that "in" works
            .register_fn("contains", |x: &mut HeaderMap, key: &str| -> bool {
                match HeaderName::from_str(key) {
                    Ok(hn) => x.contains_key(hn),
                    Err(_e) => false,
                }
            })
            // Register a HeaderMap indexer so we can get/set headers
            .register_indexer_get_result(|x: &mut HeaderMap, key: &str| {
                let search_name =
                    HeaderName::from_str(key).map_err(|e: InvalidHeaderName| e.to_string())?;
                Ok(x.get(search_name)
                    .ok_or("")?
                    .to_str()
                    .map_err(|e| e.to_string())?
                    .to_string())
            })
            .register_indexer_set_result(|x: &mut HeaderMap, key: &str, value: &str| {
                x.insert(
                    HeaderName::from_str(key).map_err(|e| e.to_string())?,
                    HeaderValue::from_str(value).map_err(|e| e.to_string())?,
                );
                Ok(())
            })
            // Register a Context indexer so we can get/set context
            .register_indexer_get_result(|x: &mut Context, key: &str| {
                x.get(key)
                    .map(|v: Option<Dynamic>| v.unwrap_or_else(|| Dynamic::from(())))
                    .map_err(|e: BoxError| e.to_string().into())
            })
            .register_indexer_set_result(|x: &mut Context, key: &str, value: Dynamic| {
                x.insert(key, value)
                    .map(|v: Option<Dynamic>| v.unwrap_or_else(|| Dynamic::from(())))
                    .map_err(|e: BoxError| e.to_string())?;
                Ok(())
            })
            // Register get for Header Name/Value from a tuple pair
            .register_get("name", |x: &mut (Option<HeaderName>, HeaderValue)| {
                x.0.clone()
            })
            .register_get("value", |x: &mut (Option<HeaderName>, HeaderValue)| {
                x.1.clone()
            })
            // Register a series of logging functions
            .register_fn("log_trace", |x: &str| {
                tracing::trace!("{}", x);
            })
            .register_fn("log_debug", |x: &str| {
                tracing::debug!("{}", x);
            })
            .register_fn("log_info", |x: &str| {
                tracing::info!("{}", x);
            })
            .register_fn("log_warn", |x: &str| {
                tracing::warn!("{}", x);
            })
            .register_fn("log_error", |x: &str| {
                tracing::error!("{}", x);
            })
            // Register a function for printing to stderr
            .register_fn("eprint", |x: &str| {
                eprintln!("{}", x);
            })
            // Default representation in rhai is the "type", so
            // we need to register a to_string function for all our registered
            // types so we can interact meaningfully with them.
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
            );

        register_rhai_interface!(engine, router, query_planner, execution, subgraph);

        engine
    }

    fn ast_has_function(&self, name: &str) -> bool {
        self.ast.iter_fn_def().any(|fn_def| fn_def.name == name)
    }
}

register_plugin!("experimental", "rhai", Rhai);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use apollo_router_core::{
        http_compat,
        plugin::utils::test::{MockExecutionService, MockRouterService},
        Context, DynPlugin, ResponseBody, RouterRequest, RouterResponse,
    };
    use serde_json::Value;
    use tower::{util::BoxService, Service, ServiceExt};

    #[tokio::test]
    async fn rhai_plugin_router_service() -> Result<(), BoxError> {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                RouterResponse::fake_builder()
                    .header("x-custom-header", "CUSTOM_VALUE")
                    .context(req.context)
                    .build()
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("experimental.rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();
        let mut router_service = dyn_plugin.router_service(BoxService::new(mock_service.build()));
        let context = Context::new();
        context.insert("test", 5i64).unwrap();
        let router_req = RouterRequest::fake_builder().context(context).build()?;

        let router_resp = router_service.ready().await?.call(router_req).await?;
        assert_eq!(router_resp.response.status(), 200);
        let headers = router_resp.response.headers().clone();
        let context = router_resp.context;
        // Check if it fails
        let body = router_resp.response.into_body();
        match body {
            ResponseBody::GraphQL(resp) => {
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
            }
            _ => {
                panic!("should not be this kind of response")
            }
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
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: ExecutionRequest| {
                Ok(ExecutionResponse::fake_builder()
                    .context(req.context)
                    .build())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("experimental.rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();
        let mut router_service =
            dyn_plugin.execution_service(BoxService::new(mock_service.build()));
        let fake_req = http_compat::Request::fake_builder()
            .header("x-custom-header", "CUSTOM_VALUE")
            .body(
                apollo_router_core::Request::builder()
                    .query(String::new())
                    .build(),
            )
            .build()?;
        let context = Context::new();
        context.insert("test", 5i64).unwrap();
        let exec_req = ExecutionRequest::fake_builder()
            .context(context)
            .originating_request(fake_req)
            .build();

        let exec_resp = router_service
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
        let body = exec_resp.response.into_body();
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
    // (there can be only one...)
    #[test]
    fn it_logs_messages() {
        let env_filter = "apollo_router=trace";
        let mock_writer =
            tracing_test::internal::MockWriter::new(&tracing_test::internal::GLOBAL_BUF);
        let subscriber = tracing_test::internal::get_subscriber(mock_writer, env_filter);

        let _guard = tracing::dispatcher::set_default(&subscriber);
        let engine = Rhai::new_rhai_engine();
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
        let engine = Rhai::new_rhai_engine();
        engine
            .eval::<()>(r#"print("info log")"#)
            .expect("it logged a message");
        assert!(tracing_test::internal::logs_with_scope_contain(
            "apollo_router",
            "info log"
        ));
    }
}
