//! Customization via Rhai.

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
use rhai::module_resolvers::FileModuleResolver;
use rhai::plugin::*;
use rhai::serde::from_dynamic;
use rhai::serde::to_dynamic;
use rhai::Dynamic;
use rhai::Engine;
use rhai::EvalAltResult;
use rhai::FnPtr;
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
use crate::register_plugin;
use crate::Context;
use crate::ExecutionRequest;
use crate::ExecutionResponse;
use crate::QueryPlannerRequest;
use crate::QueryPlannerResponse;
use crate::RouterRequest;
use crate::RouterResponse;
use crate::SubgraphRequest;
use crate::SubgraphResponse;

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

mod router {
    use super::*;

    pub(crate) type Request = RouterRequest;
    pub(crate) type Response = RhaiRouterResponse;
    pub(crate) type Service = BoxService<Request, RouterResponse, BoxError>;
}

mod query_planner {
    use super::*;

    pub(crate) type Request = QueryPlannerRequest;
    pub(crate) type Response = QueryPlannerResponse;
    pub(crate) type Service = BoxService<Request, Response, BoxError>;
}

mod execution {
    use super::*;

    pub(crate) type Request = ExecutionRequest;
    pub(crate) type Response = RhaiExecutionResponse;
    pub(crate) type Service = BoxService<Request, ExecutionResponse, BoxError>;
}

mod subgraph {
    use super::*;

    pub(crate) type Request = SubgraphRequest;
    pub(crate) type Response = SubgraphResponse;
    pub(crate) type Service = BoxService<Request, Response, BoxError>;
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
        Ok(obj.with_mut(|request| request.subgraph_request.clone()))
    }

    #[rhai_fn(set = "subgraph", return_raw)]
    pub(crate) fn set_subgraph(
        obj: &mut SharedMut<subgraph::Request>,
        sub: http_ext::Request<Request>,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|request| {
            request.subgraph_request = sub;
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
    pub(crate) fn get_originating_headers_router_response(
        obj: &mut SharedMut<router::Response>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.headers().clone()))
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_execution_response(
        obj: &mut SharedMut<execution::Response>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.headers().clone()))
    }

    #[rhai_fn(get = "headers", pure, return_raw)]
    pub(crate) fn get_originating_headers_subgraph_response(
        obj: &mut SharedMut<subgraph::Response>,
    ) -> Result<HeaderMap, Box<EvalAltResult>> {
        Ok(obj.with_mut(|response| response.response.headers().clone()))
    }

    #[rhai_fn(get = "body", pure, return_raw)]
    pub(crate) fn get_originating_body_router_response(
        obj: &mut SharedMut<router::Response>,
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

    #[rhai_fn(set = "headers", return_raw)]
    pub(crate) fn set_originating_headers_router_response(
        obj: &mut SharedMut<router::Response>,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.headers_mut() = headers);
        Ok(())
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
    pub(crate) fn set_originating_headers_subgraph_response(
        obj: &mut SharedMut<subgraph::Response>,
        headers: HeaderMap,
    ) -> Result<(), Box<EvalAltResult>> {
        obj.with_mut(|response| *response.response.headers_mut() = headers);
        Ok(())
    }

    #[rhai_fn(set = "body", return_raw)]
    pub(crate) fn set_originating_body_router_response(
        obj: &mut SharedMut<router::Response>,
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
pub struct Rhai {
    ast: AST,
    engine: Arc<Engine>,
}

/// Configuration for the Rhai Plugin
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Conf {
    scripts: Option<PathBuf>,
    main: Option<String>,
}

#[async_trait::async_trait]
impl Plugin for Rhai {
    type Config = Conf;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        let scripts_path = match configuration.scripts {
            Some(path) => path,
            None => "./rhai".into(),
        };

        let main_file = match configuration.main {
            Some(main) => main,
            None => "main.rhai".to_string(),
        };

        let main = scripts_path.join(&main_file);
        let engine = Arc::new(Rhai::new_rhai_engine(Some(scripts_path)));
        let ast = engine.compile_file(main)?;
        Ok(Self { ast, engine })
    }

    fn router_service(
        &self,
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
            None,
            ServiceStep::Router(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn query_planning_service(
        &self,
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
            None,
            ServiceStep::QueryPlanner(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn execution_service(
        &self,
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
            None,
            ServiceStep::Execution(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }

    fn subgraph_service(
        &self,
        name: &str,
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
            Some(name),
            ServiceStep::Subgraph(shared_service.clone()),
        ) {
            tracing::error!("service callback failed: {error}");
        }
        shared_service.take_unwrap()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ServiceStep {
    Router(SharedMut<router::Service>),
    QueryPlanner(SharedMut<query_planner::Service>),
    Execution(SharedMut<execution::Service>),
    Subgraph(SharedMut<subgraph::Service>),
}

// Actually use the checkpoint function so that we can shortcut requests which fail
macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $rhai_service: ident, $callback: ident) => {
        $borrow.replace(|service| {
            ServiceBuilder::new()
                .checkpoint(move |request: $base::Request| {
                    // Let's define a local function to build an error response
                    fn failure_message(
                        context: Context,
                        msg: String,
                        status: StatusCode,
                    ) -> Result<ControlFlow<$base::Response, $base::Request>, BoxError>
                    {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
                                message: msg,
                                ..Default::default()
                            }])
                            .status_code(status)
                            .context(context)
                            .build()?;
                        Ok(ControlFlow::Break(res))
                    }
                    let shared_request = Shared::new(Mutex::new(Some(request)));
                    let result: Result<Dynamic, String> = if $callback.is_curried() {
                        $callback
                            .call(
                                &$rhai_service.engine,
                                &$rhai_service.ast,
                                (shared_request.clone(),),
                            )
                            .map_err(|err| err.to_string())
                    } else {
                        let mut scope = $rhai_service.scope.clone();
                        $rhai_service
                            .engine
                            .call_fn(
                                &mut scope,
                                &$rhai_service.ast,
                                $callback.fn_name(),
                                (shared_request.clone(),),
                            )
                            .map_err(|err| err.to_string())
                    };
                    if let Err(error) = result {
                        tracing::error!("map_request callback failed: {error}");
                        let mut guard = shared_request.lock().unwrap();
                        let request_opt = guard.take();
                        return failure_message(
                            request_opt.unwrap().context,
                            format!("rhai execution error: '{}'", error),
                            StatusCode::INTERNAL_SERVER_ERROR,
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
                        msg: String,
                        status: StatusCode,
                    ) -> $base::Response {
                        let res = $base::Response::error_builder()
                            .errors(vec![Error {
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
                    let result: Result<Dynamic, String> = if $callback.is_curried() {
                        $callback
                            .call(
                                &$rhai_service.engine,
                                &$rhai_service.ast,
                                (shared_response.clone(),),
                            )
                            .map_err(|err| err.to_string())
                    } else {
                        let mut scope = $rhai_service.scope.clone();
                        $rhai_service
                            .engine
                            .call_fn(
                                &mut scope,
                                &$rhai_service.ast,
                                $callback.fn_name(),
                                (shared_response.clone(),),
                            )
                            .map_err(|err| err.to_string())
                    };
                    if let Err(error) = result {
                        tracing::error!("map_response callback failed: {error}");
                        let mut guard = shared_response.lock().unwrap();
                        let response_opt = guard.take();
                        return failure_message(
                            response_opt.unwrap().context,
                            format!("rhai execution error: '{}'", error),
                            StatusCode::INTERNAL_SERVER_ERROR,
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

pub(crate) struct RhaiExecutionResponse {
    context: Context,
    response: http_ext::Response<Response>,
}

pub(crate) struct RhaiRouterResponse {
    context: Context,
    response: http_ext::Response<Response>,
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
            $engine.register_get_result(
                "context",
                |obj: &mut SharedMut<$base::Request>| {
                    Ok(obj.with_mut(|request| request.context.clone()))
                }
            )
            .register_get_result(
                "context",
                |obj: &mut SharedMut<$base::Response>| {
                    Ok(obj.with_mut(|response| response.context.clone()))
                }
            );

            $engine.register_set_result(
                "context",
                |obj: &mut SharedMut<$base::Request>, context: Context| {
                    obj.with_mut(|request| request.context = context);
                    Ok(())
                }
            )
            .register_set_result(
                "context",
                |obj: &mut SharedMut<$base::Response>, context: Context| {
                    obj.with_mut(|response| response.context = context);
                    Ok(())
                }
            );

            // Originating Request
            $engine.register_get_result(
                "headers",
                |obj: &mut SharedMut<$base::Request>| {
                    Ok(obj.with_mut(|request| request.originating_request.headers().clone()))
                }
            );

            $engine.register_set_result(
                "headers",
                |obj: &mut SharedMut<$base::Request>, headers: HeaderMap| {
                    if_subgraph! {
                        $base => {
                            let _unused = (obj, headers);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.originating_request.headers_mut() = headers);
                            Ok(())
                        }
                    }
                }
            );

            $engine.register_get_result(
                "body",
                |obj: &mut SharedMut<$base::Request>| {
                    Ok(obj.with_mut(|request| request.originating_request.body().clone()))
                }
            );

            $engine.register_set_result(
                "body",
                |obj: &mut SharedMut<$base::Request>, body: Request| {
                    if_subgraph! {
                        $base => {
                            let _unused = (obj, body);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.originating_request.body_mut() = body);
                            Ok(())
                        }
                    }
                }
            );

            $engine.register_get_result(
                "uri",
                |obj: &mut SharedMut<$base::Request>| {
                    Ok(obj.with_mut(|request| request.originating_request.uri().clone()))
                }
            );

            $engine.register_set_result(
                "uri",
                |obj: &mut SharedMut<$base::Request>, uri: Uri| {
                    if_subgraph! {
                        $base => {
                            let _unused = (obj, uri);
                            Err("cannot mutate originating request on a subgraph".into())
                        } else {
                            obj.with_mut(|request| *request.originating_request.uri_mut() = uri);
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
            ServiceStep::Router(service) => {
                //gen_map_request!(router, service, rhai_service, callback);
                service.replace(|service| {
                    ServiceBuilder::new()
                        .checkpoint(move |request: RouterRequest| {
                            // Let's define a local function to build an error response
                            fn failure_message(
                                context: Context,
                                msg: String,
                                status: StatusCode,
                            ) -> Result<ControlFlow<RouterResponse, RouterRequest>, BoxError>
                            {
                                let res = RouterResponse::error_builder()
                                    .errors(vec![Error {
                                        message: msg,
                                        ..Default::default()
                                    }])
                                    .status_code(status)
                                    .context(context)
                                    .build()?;
                                Ok(ControlFlow::Break(res))
                            }
                            let shared_request = Shared::new(Mutex::new(Some(request)));
                            let result: Result<Dynamic, String> = if callback.is_curried() {
                                callback
                                    .call(
                                        &rhai_service.engine,
                                        &rhai_service.ast,
                                        (shared_request.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            } else {
                                let mut scope = rhai_service.scope.clone();
                                rhai_service
                                    .engine
                                    .call_fn(
                                        &mut scope,
                                        &rhai_service.ast,
                                        callback.fn_name(),
                                        (shared_request.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            };
                            if let Err(error) = result {
                                tracing::error!("map_request callback failed: {error}");
                                let mut guard = shared_request.lock().unwrap();
                                let request_opt = guard.take();
                                return failure_message(
                                    request_opt.unwrap().context,
                                    format!("rhai execution error: '{}'", error),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                );
                            }
                            let mut guard = shared_request.lock().unwrap();
                            let request_opt = guard.take();
                            Ok(ControlFlow::Continue(request_opt.unwrap()))
                        })
                        .service(service)
                        .boxed()
                })
            }
            ServiceStep::QueryPlanner(service) => {
                gen_map_request!(query_planner, service, rhai_service, callback);
            }
            ServiceStep::Execution(service) => {
                //gen_map_request!(execution, service, rhai_service, callback);
                service.replace(|service| {
                    ServiceBuilder::new()
                        .checkpoint(move |request: ExecutionRequest| {
                            // Let's define a local function to build an error response
                            fn failure_message(
                                context: Context,
                                msg: String,
                                status: StatusCode,
                            ) -> Result<ControlFlow<ExecutionResponse, ExecutionRequest>, BoxError>
                            {
                                let res = ExecutionResponse::error_builder()
                                    .errors(vec![Error {
                                        message: msg,
                                        ..Default::default()
                                    }])
                                    .status_code(status)
                                    .context(context)
                                    .build()?;
                                Ok(ControlFlow::Break(res))
                            }
                            let shared_request = Shared::new(Mutex::new(Some(request)));
                            let result: Result<Dynamic, String> = if callback.is_curried() {
                                callback
                                    .call(
                                        &rhai_service.engine,
                                        &rhai_service.ast,
                                        (shared_request.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            } else {
                                let mut scope = rhai_service.scope.clone();
                                rhai_service
                                    .engine
                                    .call_fn(
                                        &mut scope,
                                        &rhai_service.ast,
                                        callback.fn_name(),
                                        (shared_request.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            };
                            if let Err(error) = result {
                                tracing::error!("map_request callback failed: {error}");
                                let mut guard = shared_request.lock().unwrap();
                                let request_opt = guard.take();
                                return failure_message(
                                    request_opt.unwrap().context,
                                    format!("rhai execution error: '{}'", error),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                );
                            }
                            let mut guard = shared_request.lock().unwrap();
                            let request_opt = guard.take();
                            Ok(ControlFlow::Continue(request_opt.unwrap()))
                        })
                        .service(service)
                        .boxed()
                })
            }
            ServiceStep::Subgraph(service) => {
                gen_map_request!(subgraph, service, rhai_service, callback);
            }
        }
    }

    fn map_response(&mut self, rhai_service: RhaiService, callback: FnPtr) {
        match self {
            ServiceStep::Router(service) => {
                // gen_map_response!(router, service, rhai_service, callback);
                service.replace(|service| {
                    BoxService::new(service.and_then(
                        |router_response: RouterResponse| async move {
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
                            ) -> RouterResponse {
                                let res = RouterResponse::error_builder()
                                    .errors(vec![Error {
                                        message: msg,
                                        ..Default::default()
                                    }])
                                    .status_code(status)
                                    .context(context)
                                    .build()
                                    .expect("can't fail to build our error message");
                                res
                            }

                            // we split the response stream into headers+first response, then a stream of deferred responses
                            // for which we will implement mapping later
                            let RouterResponse { response, context } = router_response;
                            let (parts, stream) = http::Response::from(response).into_parts();
                            let (first, rest) = stream.into_future().await;

                            if first.is_none() {
                                return Ok(failure_message(
                                    context,
                                    "rhai execution error: empty response".to_string(),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                ));
                            }

                            let response = RhaiRouterResponse {
                                context,
                                response: http::Response::from_parts(
                                    parts,
                                    first.expect("already checked"),
                                )
                                .into(),
                            };
                            let shared_response = Shared::new(Mutex::new(Some(response)));

                            let result: Result<Dynamic, String> = if callback.is_curried() {
                                callback
                                    .call(
                                        &rhai_service.engine,
                                        &rhai_service.ast,
                                        (shared_response.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            } else {
                                let mut scope = rhai_service.scope.clone();
                                rhai_service
                                    .engine
                                    .call_fn(
                                        &mut scope,
                                        &rhai_service.ast,
                                        callback.fn_name(),
                                        (shared_response.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            };
                            if let Err(error) = result {
                                tracing::error!("map_response callback failed: {error}");
                                let mut guard = shared_response.lock().unwrap();
                                let response_opt = guard.take();
                                return Ok(failure_message(
                                    response_opt.unwrap().context,
                                    format!("rhai execution error: '{}'", error),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                ));
                            }

                            let mut guard = shared_response.lock().unwrap();
                            let response_opt = guard.take();
                            let RhaiRouterResponse { context, response } = response_opt.unwrap();
                            let (parts, body) = http::Response::from(response).into_parts();

                            //FIXME we should also map over the stream of future responses
                            let response = http::Response::from_parts(
                                parts,
                                once(ready(body)).chain(rest).boxed(),
                            )
                            .into();
                            Ok(RouterResponse { context, response })
                        },
                    ))
                })
            }
            ServiceStep::QueryPlanner(service) => {
                gen_map_response!(query_planner, service, rhai_service, callback);
            }
            ServiceStep::Execution(service) => {
                //gen_map_response!(execution, service, rhai_service, callback);
                service.replace(|service| {
                    service
                        .and_then(|execution_response: ExecutionResponse| async move {
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
                            ) -> ExecutionResponse {
                                ExecutionResponse::error_builder()
                                    .errors(vec![Error {
                                        message: msg,
                                        ..Default::default()
                                    }])
                                    .status_code(status)
                                    .context(context)
                                    .build()
                                    .expect("can't fail to build our error message")
                            }

                            // we split the response stream into headers+first response, then a stream of deferred responses
                            // for which we will implement mapping later
                            let ExecutionResponse { response, context } = execution_response;
                            let (parts, stream) = http::Response::from(response).into_parts();
                            let (first, rest) = stream.into_future().await;

                            if first.is_none() {
                                return Ok(failure_message(
                                    context,
                                    "rhai execution error: empty response".to_string(),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                ));
                            }

                            let response = RhaiExecutionResponse {
                                context,
                                response: http::Response::from_parts(
                                    parts,
                                    first.expect("already checked"),
                                )
                                .into(),
                            };
                            let shared_response = Shared::new(Mutex::new(Some(response)));
                            let result: Result<Dynamic, String> = if callback.is_curried() {
                                callback
                                    .call(
                                        &rhai_service.engine,
                                        &rhai_service.ast,
                                        (shared_response.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            } else {
                                let mut scope = rhai_service.scope.clone();
                                rhai_service
                                    .engine
                                    .call_fn(
                                        &mut scope,
                                        &rhai_service.ast,
                                        callback.fn_name(),
                                        (shared_response.clone(),),
                                    )
                                    .map_err(|err| err.to_string())
                            };
                            if let Err(error) = result {
                                tracing::error!("map_response callback failed: {error}");
                                let mut guard = shared_response.lock().unwrap();
                                let response_opt = guard.take();
                                return Ok(failure_message(
                                    response_opt.unwrap().context,
                                    format!("rhai execution error: '{}'", error),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                ));
                            }

                            let mut guard = shared_response.lock().unwrap();
                            let response_opt = guard.take();
                            let RhaiExecutionResponse { context, response } = response_opt.unwrap();
                            let (parts, body) = http::Response::from(response).into_parts();

                            //FIXME we should also map over the stream of future responses
                            let response = http::Response::from_parts(
                                parts,
                                once(ready(body)).chain(rest).boxed(),
                            )
                            .into();
                            Ok(ExecutionResponse { context, response })
                        })
                        .boxed()
                })
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
    fn run_rhai_service(
        &self,
        function_name: &str,
        subgraph: Option<&str>,
        service: ServiceStep,
    ) -> Result<(), String> {
        let mut scope = Scope::new();
        scope.push_constant("apollo_start", Instant::now());
        let rhai_service = RhaiService {
            scope: scope.clone(),
            service,
            engine: self.engine.clone(),
            ast: self.ast.clone(),
        };
        match subgraph {
            Some(name) => {
                self.engine
                    .call_fn(
                        &mut scope,
                        &self.ast,
                        function_name,
                        (rhai_service, name.to_string()),
                    )
                    .map_err(|err| err.to_string())?;
            }
            None => {
                self.engine
                    .call_fn(&mut scope, &self.ast, function_name, (rhai_service,))
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
                    .map(|v: Option<Dynamic>| v.unwrap_or(Dynamic::UNIT))
                    .map_err(|e: BoxError| e.to_string().into())
            })
            .register_indexer_set_result(|x: &mut Context, key: &str, value: Dynamic| {
                x.insert(key, value)
                    .map(|v: Option<Dynamic>| v.unwrap_or(Dynamic::UNIT))
                    .map_err(|e: BoxError| e.to_string())?;
                Ok(())
            })
            // Register Context.upsert()
            .register_result_fn(
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
            .register_get_result("variables", |x: &mut Request| {
                to_dynamic(x.variables.clone())
            })
            .register_set_result("variables", |x: &mut Request, om: Map| {
                x.variables = from_dynamic(&om.into())?;
                Ok(())
            })
            // Request.extensions
            .register_get_result("extensions", |x: &mut Request| {
                to_dynamic(x.extensions.clone())
            })
            .register_set_result("extensions", |x: &mut Request, om: Map| {
                x.extensions = from_dynamic(&om.into())?;
                Ok(())
            })
            // Request.uri.path
            .register_get_result("path", |x: &mut Uri| to_dynamic(x.path()))
            .register_set_result("path", |x: &mut Uri, value: &str| {
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
                    None => {
                        Some(PathAndQuery::from_maybe_shared(value).map_err(|e| e.to_string())?)
                    }
                };
                *x = Uri::from_parts(parts).map_err(|e| e.to_string())?;
                Ok(())
            })
            // Request.uri.host
            .register_get_result("host", |x: &mut Uri| to_dynamic(x.host()))
            .register_set_result("host", |x: &mut Uri, value: &str| {
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
                            Authority::from_maybe_shared(value).map_err(|e| e.to_string())?
                        }
                    }
                    None => Authority::from_maybe_shared(value).map_err(|e| e.to_string())?,
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
            .register_get_result("data", |x: &mut Response| to_dynamic(x.data.clone()))
            .register_set_result("data", |x: &mut Response, om: Map| {
                x.data = from_dynamic(&om.into())?;
                Ok(())
            })
            // Response.path (Not Implemented)
            // Response.errors
            .register_get_result("errors", |x: &mut Response| to_dynamic(x.errors.clone()))
            .register_set_result("errors", |x: &mut Response, value: Dynamic| {
                x.errors = from_dynamic(&value)?;
                Ok(())
            })
            // Response.extensions
            .register_get_result("extensions", |x: &mut Response| {
                to_dynamic(x.extensions.clone())
            })
            .register_set_result("extensions", |x: &mut Response, om: Map| {
                x.extensions = from_dynamic(&om.into())?;
                Ok(())
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
            .register_fn("to_string", |x: &mut Uri| -> String { format!("{:?}", x) });

        register_rhai_interface!(engine, router, execution, subgraph);

        engine
            .register_get_result("context", |obj: &mut SharedMut<query_planner::Request>| {
                Ok(obj.with_mut(|request| request.context.clone()))
            })
            .register_get_result("context", |obj: &mut SharedMut<query_planner::Response>| {
                Ok(obj.with_mut(|response| response.context.clone()))
            });

        engine
            .register_set_result(
                "context",
                |obj: &mut SharedMut<query_planner::Request>, context: Context| {
                    obj.with_mut(|request| request.context = context);
                    Ok(())
                },
            )
            .register_set_result(
                "context",
                |obj: &mut SharedMut<query_planner::Response>, context: Context| {
                    obj.with_mut(|response| response.context = context);
                    Ok(())
                },
            );

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

    use serde_json::Value;
    use tower::util::BoxService;
    use tower::Service;
    use tower::ServiceExt;

    use super::*;
    use crate::http_ext;
    use crate::plugin::test::MockExecutionService;
    use crate::plugin::test::MockRouterService;
    use crate::plugin::DynPlugin;
    use crate::Context;
    use crate::RouterRequest;
    use crate::RouterResponse;

    #[tokio::test]
    async fn rhai_plugin_router_service() -> Result<(), BoxError> {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                Ok(RouterResponse::fake_builder()
                    .header("x-custom-header", "CUSTOM_VALUE")
                    .context(req.context)
                    .build()
                    .unwrap())
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();
        let mut router_service = dyn_plugin.router_service(BoxService::new(mock_service.build()));
        let context = Context::new();
        context.insert("test", 5i64).unwrap();
        let router_req = RouterRequest::fake_builder().context(context).build()?;

        let mut router_resp = router_service.ready().await?.call(router_req).await?;
        assert_eq!(router_resp.response.status(), 200);
        let headers = router_resp.response.headers().clone();
        let context = router_resp.context.clone();
        // Check if it fails
        let resp = router_resp.next_response().await.unwrap();
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
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: ExecutionRequest| {
                Ok(ExecutionResponse::fake_builder()
                    .context(req.context)
                    .build())
            });

        let dyn_plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
            .get("apollo.rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"scripts":"tests/fixtures", "main":"test.rhai"}"#).unwrap(),
            )
            .await
            .unwrap();
        let mut router_service =
            dyn_plugin.execution_service(BoxService::new(mock_service.build()));
        let fake_req = http_ext::Request::fake_builder()
            .header("x-custom-header", "CUSTOM_VALUE")
            .body(Request::builder().query(String::new()).build())
            .build()?;
        let context = Context::new();
        context.insert("test", 5i64).unwrap();
        let exec_req = ExecutionRequest::fake_builder()
            .context(context)
            .originating_request(fake_req)
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
}
