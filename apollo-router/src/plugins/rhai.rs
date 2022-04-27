//! Customization via Rhai.

use std::sync::Mutex;
use std::{path::PathBuf, str::FromStr, sync::Arc};

use apollo_router_core::{register_plugin, Object, Plugin, RouterRequest, RouterResponse, Value};
use apollo_router_core::{
    Context, Entries, ExecutionRequest, ExecutionResponse, QueryPlannerRequest,
    QueryPlannerResponse, SubgraphRequest, SubgraphResponse,
};
use http::HeaderMap;
use http::{header::HeaderName, HeaderValue};
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Dynamic, Engine, EvalAltResult, Scope, Shared, AST};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;
use tower::{util::BoxService, BoxError, ServiceExt};

use rhai::plugin::*; // a "prelude" import for macros

use rhai::{FnPtr, NativeCallContext};

#[export_module]
mod rhai_plugin_mod {

    // This is a setter for 'RouterRequest::context'.
    #[rhai_fn(set = "context")]
    pub fn set_context(obj: &mut SharedRouterRequest, value: Context) {
        let mut guard = obj.lock().unwrap();
        let request_opt = guard.take();
        match request_opt {
            Some(mut request) => {
                request.context = value;
                guard.replace(request);
            }
            None => panic!("surely there is a request here..."),
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

    // This is part of our 'RouterRequest::context'.
    #[rhai_fn(return_raw)]
    pub fn get_context(
        obj: &mut SharedRouterRequest,
        key: &str,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let mut guard = obj.lock().unwrap();
        let request_opt = guard.take();
        match request_opt {
            Some(request) => {
                let result = request
                    .context
                    .get(key)
                    .map(|v: Option<Dynamic>| v.unwrap_or_else(|| Dynamic::from(())))
                    .map_err(|e: BoxError| e.to_string().into());
                guard.replace(request);
                result
            }
            None => panic!("surely there is a request here..."),
        }
    }

    // This is part of our 'RouterRequest::context'.
    #[rhai_fn(return_raw)]
    pub fn insert_context(
        obj: &mut SharedRouterRequest,
        key: &str,
        value: Dynamic,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let mut guard = obj.lock().unwrap();
        let request_opt = guard.take();
        match request_opt {
            Some(request) => {
                let result = request
                    .context
                    .insert(key, value)
                    .map(|v: Option<Dynamic>| v.unwrap_or_else(|| Dynamic::from(())))
                    .map_err(|e: BoxError| e.to_string().into());
                guard.replace(request);
                result
            }
            None => panic!("surely there is a request here..."),
        }
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

trait RhaiObjectSetterGetter {
    fn set(&mut self, key: String, value: Value);
    fn get_cloned(&mut self, key: String) -> Value;
}

impl RhaiObjectSetterGetter for Object {
    fn set(&mut self, key: String, value: Value) {
        self.insert(ByteString::from(key), value);
    }
    fn get_cloned(&mut self, key: String) -> Value {
        self.get(&ByteString::from(key))
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
struct Headers(HeaderMap);
impl Headers {
    fn set_header(&mut self, name: String, value: String) -> Result<(), Box<EvalAltResult>> {
        self.0.insert(
            HeaderName::from_str(&name)
                .map_err(|err| format!("invalid header name '{name}': {err}"))?,
            HeaderValue::from_str(&value)
                .map_err(|err| format!("invalid header value '{value}': {err}"))?,
        );

        Ok(())
    }
    fn get_header(&mut self, name: String) -> String {
        self.0
            .get(&name)
            .cloned()
            .and_then(|h| Some(h.to_str().ok()?.to_string()))
            .unwrap_or_default()
    }
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
        let step = ServiceStep::Router(shared_service.clone());
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
        service = step.extract_router_service();

        service
    }

    fn query_planning_service(
        &mut self,
        mut service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "query_planner_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        let step = ServiceStep::QueryPlanner(shared_service.clone());
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
        service = step.extract_query_planner_service();

        service
    }

    fn execution_service(
        &mut self,
        mut service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "execution_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        let step = ServiceStep::Execution(shared_service.clone());
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
        service = step.extract_execution_service();

        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        mut service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const FUNCTION_NAME_SERVICE: &str = "subgraph_service";
        let shared_service = Arc::new(Mutex::new(Some(service)));
        let step = ServiceStep::Subgraph(shared_service.clone());
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
        service = step.extract_subgraph_service();

        service
    }
}

impl RhaiObjectSetterGetter for Entries {
    fn set(&mut self, key: String, value: Value) {
        self.insert(key, value);
    }
    fn get_cloned(&mut self, key: String) -> Value {
        self.get(&key).map(|v| v.clone()).unwrap_or_default()
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
    ($base: ident) => {
        paste::paste! {
            #[allow(dead_code)]
            type [<Shared $base:camel Service>] = Arc<Mutex<Option<BoxService<[<$base:camel Request>], [<$base:camel Response>], BoxError>>>>;

            #[allow(dead_code)]
            type [<Shared $base:camel Request>] = Arc<Mutex<Option<[<$base:camel Request>]>>>;

            #[allow(dead_code)]
            type [<Shared $base:camel Response>] = Arc<Mutex<Option<[<$base:camel Response>]>>>;
        }
    };
}

macro_rules! gen_service_step {
    ($base: ident) => {
        paste::paste! {
            fn [<extract_ $base _service>](self) -> BoxService<[<$base:camel Request>], [<$base:camel Response>], BoxError> {
                match self {
                    ServiceStep::[<$base:camel>](v) => v.lock().unwrap().take().unwrap(),
                    _ => panic!("XXX Figure this out at some point"),
                }
            }

            fn [<borrow_ $base _service>](&mut self) -> [<Shared $base:camel Service>] {
                match self {
                    ServiceStep::[<$base:camel>](v) => v.clone(),
                    _ => panic!("XXX Figure this out at some point"),
                }
            }
        }
    };
}

macro_rules! gen_map_request {
    ($base: ident, $this: ident, $engine: ident, $ast: ident, $callback: ident) => {
        paste::paste! {
            let borrow = $this.[<borrow_ $base _service>]();
            let mut guard = borrow.lock().unwrap();
            let service_opt = guard.take();
            match service_opt {
                Some(service) => {
                    let new_service = service
                        .map_request(move |request: [<$base:camel Request>]| {
                            let shared_request = Shared::new(Mutex::new(Some(request)));
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
    ($base: ident, $this: ident, $engine: ident, $ast: ident, $callback: ident) => {
        paste::paste! {
            let borrow = $this.[<borrow_ $base _service>]();
            let mut guard = borrow.lock().unwrap();
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

gen_shared_types!(router);
gen_shared_types!(query_planner);
gen_shared_types!(execution);
gen_shared_types!(subgraph);

impl ServiceStep {
    gen_service_step!(router);
    gen_service_step!(query_planner);
    gen_service_step!(execution);
    gen_service_step!(subgraph);

    fn map_request(&mut self, engine: Arc<Engine>, ast: AST, callback: FnPtr) {
        match self {
            ServiceStep::Router(_) => {
                gen_map_request!(router, self, engine, ast, callback);
            }
            ServiceStep::QueryPlanner(_) => {
                gen_map_request!(query_planner, self, engine, ast, callback);
            }
            ServiceStep::Execution(_) => {
                gen_map_request!(execution, self, engine, ast, callback);
            }
            ServiceStep::Subgraph(_) => {
                gen_map_request!(subgraph, self, engine, ast, callback);
            }
        }
    }

    fn map_response(&mut self, engine: Arc<Engine>, ast: AST, callback: FnPtr) {
        match self {
            ServiceStep::Router(_) => {
                gen_map_response!(router, self, engine, ast, callback);
            }
            ServiceStep::QueryPlanner(_) => {
                gen_map_response!(query_planner, self, engine, ast, callback);
            }
            ServiceStep::Execution(_) => {
                gen_map_response!(execution, self, engine, ast, callback);
            }
            ServiceStep::Subgraph(_) => {
                gen_map_response!(subgraph, self, engine, ast, callback);
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

#[derive(Clone, Debug)]
pub(crate) struct RhaiContext {
    headers: HeaderMap,
    context: Context,
}

impl RhaiContext {
    fn get_headers(&mut self) -> Headers {
        Headers(self.headers.clone())
    }
    fn set_headers(&mut self, headers: Headers) {
        self.headers = headers.0;
    }
    fn get_entries(&mut self) -> Dynamic {
        to_dynamic(self.context.entries.clone()).unwrap()
    }
    fn set_entries(&mut self, entries: Dynamic) {
        self.context.entries = from_dynamic(&entries).unwrap();
    }
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
        engine.register_global_module(module.into());

        engine
            .set_max_expr_depths(0, 0)
            .register_type::<SharedRouterRequest>()
            .register_type::<RhaiContext>()
            .register_type::<Context>()
            .register_indexer_set_result(Headers::set_header)
            .register_indexer_get(Headers::get_header)
            .register_indexer_set(Object::set)
            .register_indexer_get(Object::get_cloned)
            .register_indexer_set(Entries::set)
            .register_indexer_get(Entries::get_cloned)
            .register_get_set(
                "headers",
                RhaiContext::get_headers,
                RhaiContext::set_headers,
            )
            .register_get_set(
                "entries",
                RhaiContext::get_entries,
                RhaiContext::set_entries,
            );

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
