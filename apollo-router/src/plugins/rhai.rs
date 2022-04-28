//! Customization via Rhai.

use apollo_router_core::{
    http_compat, register_plugin, Context, ExecutionRequest, ExecutionResponse, Plugin,
    QueryPlannerRequest, QueryPlannerResponse, Request, RouterRequest, RouterResponse,
    SubgraphRequest, SubgraphResponse,
};
use http::header::{HeaderName, HeaderValue};
use http::HeaderMap;
use rhai::{plugin::*, Dynamic, Engine, EvalAltResult, FnPtr, Scope, Shared, AST};
use schemars::JsonSchema;
use serde::Deserialize;
use std::str::FromStr;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::{util::BoxService, BoxError, ServiceExt};

pub trait Accessor<Access>: Send {
    fn accessor(&self) -> &Access;

    fn accessor_mut(&mut self) -> &mut Access;
}

#[export_module]
mod rhai_plugin_mod {
    macro_rules! gen_rhai_interface {
        ($ ($base: ident), +) => {
            #[export_module]
            pub(crate) mod rhai_generated_mod {
                $(
            paste::paste! {
                pub fn [<get_context_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                    key: &str,
                ) -> Result<Dynamic, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let request_opt = guard.take();
                    match request_opt {
                        Some(mut request) => {
                            let result = get_context(&mut request, key);
                            guard.replace(request);
                            result
                        }
                        None => panic!("surely there is a request here..."),
                    }
                }

                pub fn [<get_context_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                    key: &str,
                ) -> Result<Dynamic, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let response_opt = guard.take();
                    match response_opt {
                        Some(mut response) => {
                            let result = get_context(&mut response, key);
                            guard.replace(response);
                            result
                        }
                        None => panic!("surely there is a response here..."),
                    }
                }

                pub fn [<insert_context_ $base _request>](
                    obj: &mut [<Shared $base:camel Request>],
                    key: &str,
                    value: Dynamic,
                ) -> Result<Dynamic, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let request_opt = guard.take();
                    match request_opt {
                        Some(mut request) => {
                            let result = insert_context(&mut request, key, value);
                            guard.replace(request);
                            result
                        }
                        None => panic!("surely there is a request here..."),
                    }
                }

                pub fn [<insert_context_ $base _response>](
                    obj: &mut [<Shared $base:camel Response>],
                    key: &str,
                    value: Dynamic,
                ) -> Result<Dynamic, Box<EvalAltResult>> {
                    let mut guard = obj.lock().unwrap();
                    let response_opt = guard.take();
                    match response_opt {
                        Some(mut response) => {
                            let result = insert_context(&mut response, key, value);
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
            }
                )*
            }
        };
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

    fn get_context<T: Accessor<Context>>(
        obj: &mut T,
        key: &str,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        obj.accessor()
            .get(key)
            .map(|v: Option<Dynamic>| v.unwrap_or_else(|| Dynamic::from(())))
            .map_err(|e: BoxError| e.to_string().into())
    }

    fn insert_context<T: Accessor<Context>>(
        obj: &mut T,
        key: &str,
        value: Dynamic,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        obj.accessor_mut()
            .insert(key, value)
            .map(|v: Option<Dynamic>| v.unwrap_or_else(|| Dynamic::from(())))
            .map_err(|e: BoxError| e.to_string().into())
    }

    fn get_originating_headers<T: Accessor<http_compat::Request<Request>>>(
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
        /*
        Ok(obj
            .accessor()
            .headers()
            .iter()
            .map(|(name, _value)| {
                // vec![
                Dynamic::from(name.to_string()) /*
                                                name.to_string(),
                                                value
                                                    .to_str()
                                                    .expect("XXX HEADER VALUES SHOULD BE STRINGS")
                                                    .to_string(),
                                                    */
                // ]
            })
            // .map_err(|e: BoxError| e.to_string().into())
            .collect::<Vec<Dynamic>>())
                */
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
            let this = self.clone();
            tracing::debug!("router_service function found");

            this.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::Router(shared_service.clone()),
            )
            .expect("XXX FIX THIS");
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
            let this = self.clone();
            tracing::debug!("query_planner_service function found");

            this.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::QueryPlanner(shared_service.clone()),
            )
            .expect("XXX FIX THIS");
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
            let this = self.clone();
            tracing::debug!("execution_service function found");

            this.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::Execution(shared_service.clone()),
            )
            .expect("XXX FIX THIS");
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
            let this = self.clone();
            tracing::debug!("subgraph_service function found");

            this.run_rhai_service(
                FUNCTION_NAME_SERVICE,
                ServiceStep::Subgraph(shared_service.clone()),
            )
            .expect("XXX FIX THIS");
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

macro_rules! gen_map_request {
    ($base: ident, $borrow: ident, $engine: ident, $ast: ident, $callback: ident) => {
        paste::paste! {
            let mut guard = $borrow.lock().unwrap();
            let service_opt = guard.take();
            match service_opt {
                Some(service) => {
                    let new_service = service
                        .map_request(move |request: [<$base:camel Request>]| {
                            let shared_request = Shared::new(Mutex::new(Some(request)));
                            // let boxed_request = Box::new(request) as Box<dyn ContextAccessor<Accessor = Context>>;
                            // let shared_request = Shared::new(Mutex::new(Some(boxed_request)));
                            let result: Result<Dynamic, String> = $callback
                                .call(&$engine, &$ast, (shared_request.clone(),))
                                .map_err(|err| err.to_string());
                            if let Err(error) = result {
                                tracing::error!("map_request callback failed: {error}");
                            }
                            let mut guard = shared_request.lock().unwrap();
                            let request_opt = guard.take();
                            request_opt.unwrap()
                        })
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
                            let shared_response = Shared::new(Mutex::new(Some(response)));
                            let result: Result<Dynamic, String> = $callback
                                .call(&$engine, &$ast, (shared_response.clone(),))
                                .map_err(|err| err.to_string());
                            if let Err(error) = result {
                                tracing::error!("map_response callback failed: {error}");
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

macro_rules! register_rhai_interface {
    ($engine: ident, $($base: ident), *) => {
        $(
            paste::paste! {
            // Context stuff
            $engine.register_result_fn(
                "get_context",
                rhai_plugin_mod::rhai_generated_mod::[<get_context_ $base _request>],
            )
            .register_result_fn(
                "get_context",
                rhai_plugin_mod::rhai_generated_mod::[<get_context_ $base _response>],
            );
            $engine.register_result_fn(
                "insert_context",
                rhai_plugin_mod::rhai_generated_mod::[<insert_context_ $base _request>],
            )
            .register_result_fn(
                "insert_context",
                rhai_plugin_mod::rhai_generated_mod::[<insert_context_ $base _response>],
            );

            // Originating Request
            $engine.register_get_result(
                "headers",
                rhai_plugin_mod::rhai_generated_mod::[<get_originating_headers_ $base _request>],
            );

            $engine.register_set_result(
                "headers",
                rhai_plugin_mod::rhai_generated_mod::[<set_originating_headers_ $base _request>],
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
    fn run_rhai_service(
        &self,
        function_name: &str,
        service: ServiceStep,
    ) -> Result<String, String> {
        let mut scope = Scope::new();
        let rhai_service = RhaiService {
            service,
            engine: self.engine.clone(),
            ast: self.ast.clone(),
        };
        let response: String = self
            .engine
            .call_fn(&mut scope, &self.ast, function_name, (rhai_service,))
            .map_err(|err| err.to_string())?;

        Ok(response)
    }

    fn new_rhai_engine() -> Engine {
        let mut engine = Engine::new();

        // The macro call creates a Rhai module from the plugin module.
        let module = exported_module!(rhai_plugin_mod);

        // A module can simply be registered into the global namespace.
        engine
            .register_global_module(module.into())
            .register_type::<SharedRouterRequest>()
            .register_fn("to_string", |x: &mut SharedRouterRequest| -> String {
                format!(
                    "{:?}",
                    x.lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .originating_request
                        .body()
                        .operation_name
                )
            })
            .register_type::<HeaderMap>()
            .register_fn("insert", |x: &mut HeaderMap, name: &str, value: &str| {
                x.insert(
                    HeaderName::from_str(name).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            })
            .register_type::<Option<HeaderName>>()
            .register_type::<HeaderName>()
            .register_type::<HeaderValue>()
            .register_get("name", |x: &mut (Option<HeaderName>, HeaderValue)| {
                x.0.clone()
            })
            .register_get("value", |x: &mut (Option<HeaderName>, HeaderValue)| {
                x.1.clone()
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
                x.to_str().expect("XXX").to_string()
            })
            .register_iterator::<HeaderMap>()
            .register_fn("to_string", |x: &mut HeaderMap| -> String {
                let mut msg = String::new();
                for pair in x.iter() {
                    let line = format!(
                        "{}: {}",
                        pair.0.to_string(),
                        pair.1.to_str().expect("XXX").to_string()
                    );
                    msg.push_str(line.as_ref());
                    msg.push_str("\n");
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
                        x.1.to_str().expect("XXX").to_string()
                    )
                },
            )
            .register_type::<(Option<HeaderName>, HeaderValue)>();

        engine.set_max_expr_depths(0, 0);
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
        assert_eq!(exec_resp.response.status(), 200);
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
            "RHAI plugin error: Runtime error: An error occured (line 25, position 5) in call to function execution_service_request"
        );
        Ok(())
    }
}
