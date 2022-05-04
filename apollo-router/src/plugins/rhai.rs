//! Customization via Rhai.

use apollo_router_core::{
    http_compat, register_plugin, Context, ExecutionRequest, ExecutionResponse, Plugin,
    QueryPlannerRequest, QueryPlannerResponse, Request, ResponseBody, RouterRequest,
    RouterResponse, ServiceBuilderExt, SubgraphRequest, SubgraphResponse,
};
use http::header::{HeaderName, HeaderValue, InvalidHeaderName};
use http::{HeaderMap, StatusCode};
use rhai::{plugin::*, Dynamic, Engine, EvalAltResult, FnPtr, Scope, Shared, AST};
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

#[export_module]
mod router_plugin_mod {
    macro_rules! gen_rhai_interface {
        ($ ($base: ident), +) => {
            #[export_module]
            pub(crate) mod router_generated_mod {
                $(
            paste::paste! {
                pub fn [<get_context_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                ) -> Result<Context, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let request_opt = guard.take();
                    match request_opt {
                        Some(mut request) => {
                            let result = get_context(&mut request);
                            guard.replace(request);
                            result
                        }
                        None => panic!("surely there is a request here..."),
                    }
                }

                pub fn [<get_context_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                ) -> Result<Context, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let response_opt = guard.take();
                    match response_opt {
                        Some(mut response) => {
                            let result = get_context(&mut response);
                            guard.replace(response);
                            result
                        }
                        None => panic!("surely there is a response here..."),
                    }
                }

                pub fn [<insert_context_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                    context: Context
                ) -> Result<(), Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let request_opt = guard.take();
                    match request_opt {
                        Some(mut request) => {
                            let result = insert_context(&mut request, context);
                            guard.replace(request);
                            result
                        }
                        None => panic!("surely there is a request here..."),
                    }
                }

                pub fn [<insert_context_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                    context: Context
                ) -> Result<(), Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let response_opt = guard.take();
                    match response_opt {
                        Some(mut response) => {
                            let result = insert_context(&mut response, context);
                            guard.replace(response);
                            result
                        }
                        None => panic!("surely there is a response here..."),
                    }
                }

                pub fn [<get_originating_headers_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                ) -> Result<HeaderMap, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let request_opt = guard.take();
                    match request_opt {
                        Some(mut request) => {
                            let result = get_originating_headers(&mut request);
                            guard.replace(request);
                            result
                        }
                        None => panic!("surely there is a request here..."),
                    }
                }

                /* XXX ONLY VALID FOR CERTAIN TYPES
                 *
                pub fn [<get_originating_headers_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                ) -> Result<HeaderMap, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let response_opt = guard.take();
                    match response_opt {
                        Some(mut response) => {
                            let result = get_originating_headers(&mut response);
                            guard.replace(response);
                            result
                        }
                        None => panic!("surely there is a response here..."),
                    }
                }
                */

                pub fn [<set_originating_headers_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                    headers: HeaderMap
                ) -> Result<(), Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let request_opt = guard.take();
                    match request_opt {
                        Some(mut request) => {
                            let result = set_originating_headers(&mut request, headers);
                            guard.replace(request);
                            result
                        }
                        None => panic!("surely there is a request here..."),
                    }
                }

                /* XXX ONLY VALID FOR CERTAIN TYPES
                pub fn [<set_originating_headers_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                    headers: HeaderMap
                ) -> Result<(), Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let response_opt = guard.take();
                    match response_opt {
                        Some(mut response) => {
                            let result = set_originating_headers(&mut response, headers);
                            guard.replace(response);
                            result
                        }
                        None => panic!("surely there is a response here..."),
                    }
                }
                */
            }
                )*
            }
        };
    }

    #[rhai_fn(get = "headers", return_raw)]
    pub fn get_originating_headers_router_response(
        obj: &mut SharedRouterResponse,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        let mut guard = obj.lock().unwrap();
        let response_opt = guard.take();
        match response_opt {
            Some(mut response) => {
                let result = get_originating_headers_response_response_body(&mut response);
                guard.replace(response);
                result
            }
            None => panic!("surely there is a response here..."),
        }
    }

    #[rhai_fn(set = "headers", return_raw)]
    pub fn set_originating_headers_router_response(
        obj: &mut SharedRouterResponse,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        let mut guard = obj.lock().unwrap();
        let response_opt = guard.take();
        match response_opt {
            Some(mut response) => {
                let result = set_originating_headers_response_response_body(&mut response, headers);
                guard.replace(response);
                result
            }
            None => panic!("surely there is a response here..."),
        }
    }

    // This is a getter for 'RouterRequest::operation_name'.
    #[rhai_fn(get = "operation_name")]
    pub fn get_operation_name(obj: &mut SharedRouterRequest) -> Dynamic {
        let mut guard = obj.lock().unwrap();
        let request_opt = guard.take();
        match request_opt {
            Some(request) => {
                let result = request
                    .originating_request
                    .body()
                    .operation_name
                    .clone()
                    .map_or(Dynamic::from(()), Dynamic::from);
                guard.replace(request);
                result
            }
            None => panic!("surely there is a request here..."),
        }
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
        rhai_service.service.map_request(
            rhai_service.engine.clone(),
            rhai_service.ast.clone(),
            callback,
        )
    }

    pub(crate) fn map_response(rhai_service: &mut RhaiService, callback: FnPtr) {
        rhai_service.service.map_response(
            rhai_service.engine.clone(),
            rhai_service.ast.clone(),
            callback,
        )
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
        mut service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "router_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_SERVICE)
        {
            tracing::debug!("router_service function found");

            if let Err(error) = self.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::Router(shared_service.clone()),
            ) {
                tracing::error!("service callback failed: {error}");
            }
        }
        service = shared_service.lock().unwrap().take().unwrap();

        service
    }

    fn query_planning_service(
        &mut self,
        mut service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "query_planner_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_SERVICE)
        {
            tracing::debug!("query_planner_service function found");

            if let Err(error) = self.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::QueryPlanner(shared_service.clone()),
            ) {
                tracing::error!("service callback failed: {error}");
            }
        }
        service = shared_service.lock().unwrap().take().unwrap();

        service
    }

    fn execution_service(
        &mut self,
        mut service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "execution_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_SERVICE)
        {
            tracing::debug!("execution_service function found");

            if let Err(error) = self.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::Execution(shared_service.clone()),
            ) {
                tracing::error!("service callback failed: {error}");
            }
        }
        service = shared_service.lock().unwrap().take().unwrap();

        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        mut service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "subgraph_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_SERVICE)
        {
            tracing::debug!("subgraph_service function found");

            if let Err(error) = self.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::Subgraph(shared_service.clone()),
            ) {
                tracing::error!("service callback failed: {error}");
            }
        }
        service = shared_service.lock().unwrap().take().unwrap();

        service
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceStep {
    Router(SharedRouterService),
    QueryPlanner(SharedQueryPlannerService),
    Execution(SharedExecutionService),
    Subgraph(SharedSubgraphService),
}

macro_rules! gen_shared_types {
    (subgraph) => {
            #[allow(dead_code)]
            type SharedSubgraphService = Arc<Mutex<Option<BoxService<SubgraphRequest, SubgraphResponse, BoxError>>>>;

            #[allow(dead_code)]
            type SharedSubgraphRequest = Arc<Mutex<Option<SubgraphRequest>>>;

            #[allow(dead_code)]
            type SharedSubgraphResponse = Arc<Mutex<Option<SubgraphResponse>>>;

            impl Accessor<Context> for SubgraphRequest {

                fn accessor(&self) -> &Context {
                    &self.context
                }

                fn accessor_mut(&mut self) -> &mut Context {
                    &mut self.context
                }
            }

            impl Accessor<Context> for SubgraphResponse {

                fn accessor(&self) -> &Context {
                    &self.context
                }

                fn accessor_mut(&mut self) -> &mut Context {
                    &mut self.context
                }
            }

            impl Accessor<http_compat::Request<Request>> for SubgraphRequest {

                fn accessor(&self) -> &http_compat::Request<Request> {
                    &self.originating_request
                }

                // XXX CAN'T DO THIS FOR SUBGRAPH
                fn accessor_mut(&mut self) -> &mut http_compat::Request<Request> {
                    panic!("Can't do this for subgraph");
                }
            }
    };
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

                fn accessor_mut(&mut self) -> &mut http_compat::Request<Request> {
                    &mut self.originating_request
                }
            }
        }
        )*
    };
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $engine: ident, $ast: ident, $callback: ident) => {
        paste::paste! {
            let mut guard = $borrow.lock().unwrap();
            let service_opt = guard.take();
            match service_opt {
                Some(service) => {
                    let new_service = ServiceBuilder::new()
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
                            let result: Result<Dynamic, String> = $callback
                                .call(&$engine, &$ast, (shared_request.clone(),))
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
                        .boxed();
                    guard.replace(new_service);
                }
                None => panic!("surely there is a service here..."),
            }
        }
    };
}

macro_rules! gen_map_response {
    ($base: ident, $borrow: ident, $engine: ident, $ast: ident, $callback: ident) => {
        paste::paste! {
            let mut guard = $borrow.lock().unwrap();
            let service_opt = guard.take();
            match service_opt {
                Some(service) => {
                    let new_service = service
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
                            let result: Result<Dynamic, String> = $callback
                                .call(&$engine, &$ast, (shared_response.clone(),))
                                .map_err(|err| err.to_string());
                            if let Err(error) = result {
                                tracing::error!("map_response callback failed: {error}");
                                eprintln!("map_response callback failed: {error}");
                                let mut guard = shared_response.lock().unwrap();
                                let response_opt = guard.take();
                                return failure_message(response_opt.unwrap().context, format!("rhai execution error: '{}'", error), StatusCode::INTERNAL_SERVER_ERROR);
                            }
                            let mut guard = shared_response.lock().unwrap();
                            let response_opt = guard.take();
                            response_opt.unwrap()
                        })
                        .boxed();
                    guard.replace(new_service);
                }
                None => panic!("surely there is a service here..."),
            }
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
    fn map_request(&mut self, engine: Arc<Engine>, ast: AST, callback: FnPtr) {
        match self {
            ServiceStep::Router(service) => {
                gen_map_request!(router, service, engine, ast, callback);
            }
            ServiceStep::QueryPlanner(service) => {
                gen_map_request!(query_planner, service, engine, ast, callback);
            }
            ServiceStep::Execution(service) => {
                gen_map_request!(execution, service, engine, ast, callback);
            }
            ServiceStep::Subgraph(service) => {
                gen_map_request!(subgraph, service, engine, ast, callback);
            }
        }
    }

    fn map_response(&mut self, engine: Arc<Engine>, ast: AST, callback: FnPtr) {
        match self {
            ServiceStep::Router(service) => {
                gen_map_response!(router, service, engine, ast, callback);
            }
            ServiceStep::QueryPlanner(service) => {
                gen_map_response!(query_planner, service, engine, ast, callback);
            }
            ServiceStep::Execution(service) => {
                gen_map_response!(execution, service, engine, ast, callback);
            }
            ServiceStep::Subgraph(service) => {
                gen_map_response!(subgraph, service, engine, ast, callback);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RhaiService {
    service: ServiceStep,
    engine: Arc<Engine>,
    ast: AST,
}

impl Rhai {
    fn run_rhai_service(&self, function_name: &str, service: ServiceStep) -> Result<(), String> {
        let mut scope = Scope::new();
        let rhai_service = RhaiService {
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
                x.contains_key(HeaderName::from_str(key).unwrap())
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
            .register_indexer_set(|x: &mut HeaderMap, key: &str, value: &str| {
                x.insert(
                    HeaderName::from_str(key).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
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
        /* XXX NO WAY TO PROPAGATE ERRORS YET
        // Check if it fails
        let body = exec_resp.response.into_body();
        eprintln!("BODY: {:?}", body);
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
            "RHAI plugin error: Runtime error: An error occured (line 25, position 5) in call to function execution_service_request"
        );
        */
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
