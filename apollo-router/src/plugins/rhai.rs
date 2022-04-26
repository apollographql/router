//! Customization via Rhai.

use std::{path::PathBuf, str::FromStr, sync::Arc};

use apollo_router_core::{
    register_plugin, Error, Object, Plugin, ResponseBody, RouterRequest, RouterResponse, Value,
};
use apollo_router_core::{
    Context, Entries, ExecutionRequest, ExecutionResponse, QueryPlannerRequest,
    QueryPlannerResponse, SubgraphRequest, SubgraphResponse,
};
use http::header::CONTENT_LENGTH;
use http::HeaderMap;
use http::{header::HeaderName, HeaderValue};
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Dynamic, Engine, EvalAltResult, Scope, AST};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;
use tower::{util::BoxService, BoxError, ServiceExt};

const CONTEXT_ERROR: &str = "__rhai_error";

macro_rules! service_handle_response {
    ($self: expr, $service: expr, $function_name: expr, $response_ty: ident) => {
        let this = $self.clone();
        let function_found = $self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == $function_name);
        $service = $service
            .map_response(move |mut response: $response_ty| {
                let previous_err: Option<String> = response
                    .context
                    .get(CONTEXT_ERROR)
                    .expect("we put the context error ourself so it will be deserializable; qed");
                if let Some(err) = previous_err {
                    return $response_ty::builder()
                        .errors(vec![Error::builder()
                            .message(format!("RHAI plugin error: {}", err.as_str()))
                            .build()])
                        .context(response.context)
                        .extensions(Object::default())
                        .build();
                }
                if function_found {
                    let rhai_context = match this.run_rhai_script(
                        $function_name,
                        response.context,
                        response.response.headers().clone(),
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            let context = Context::new();
                            context
                                .insert(CONTEXT_ERROR, err)
                                .expect("error is always a string; qed");

                            return $response_ty::builder()
                                .context(context)
                                .extensions(Object::default())
                                .build();
                        }
                    };
                    response.context = rhai_context.context;
                    *response.response.headers_mut() = rhai_context.headers;

                    response.response.headers_mut().remove(CONTENT_LENGTH);
                }

                response
            })
            .boxed();
    };
}

macro_rules! handle_error {
    ($call: expr, $req: expr) => {
        match $call {
            Ok(res) => res,
            Err(err) => {
                $req.context
                    .insert(CONTEXT_ERROR, err)
                    .expect("we manually created error string; qed");
                return $req;
            }
        }
    };
}

use rhai::plugin::*; // a "prelude" import for macros

#[export_module]
mod rhai_plugin_mod {
    // use super::RhaiContext;

    //     pub(crate) fn get_operation_name(context: &mut RhaiContext) -> String {
    //         match &context.originating_request.body().operation_name {
    //             Some(n) => n.clone(),
    //             None => "".to_string(),
    //         }
    //     }
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
        const FUNCTION_NAME_REQUEST: &str = "router_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            let this = self.clone();
            tracing::debug!("router_service_request function found");

            service = service
                .map_request(move |mut request: RouterRequest| {
                    let rhai_context = handle_error!(
                        this.run_rhai_script(
                            FUNCTION_NAME_REQUEST,
                            request.context.clone(),
                            request.originating_request.headers().clone()
                        ),
                        request
                    );
                    request.context = rhai_context.context;
                    *request.originating_request.headers_mut() = rhai_context.headers;
                    request
                        .originating_request
                        .headers_mut()
                        .remove(CONTENT_LENGTH);

                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "router_service_response";
        let this = self.clone();
        let function_found = self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_RESPONSE);
        service = service
            .map_response(move |mut response: RouterResponse| {
                let previous_err: Option<String> = response
                    .context
                    .get(CONTEXT_ERROR)
                    .expect("we put the context error ourself so it will be deserializable; qed");
                if let Some(err) = previous_err {
                    response.response = response.response.map(|body| match body {
                        ResponseBody::GraphQL(mut res) => {
                            res.errors.push(
                                Error::builder()
                                    .message(format!("RHAI plugin error: {}", err.as_str()))
                                    .build(),
                            );

                            ResponseBody::GraphQL(res)
                        }
                        _ => body,
                    });

                    return response;
                }
                if function_found {
                    let rhai_context = match this.run_rhai_script(
                        FUNCTION_NAME_RESPONSE,
                        response.context,
                        response.response.headers().clone(),
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            let context = Context::new();
                            context
                                .insert(CONTEXT_ERROR, err)
                                .expect("error is always a string; qed");

                            return RouterResponse::new_from_response(response.response, context);
                        }
                    };
                    response.context = rhai_context.context;
                    *response.response.headers_mut() = rhai_context.headers;

                    response.response.headers_mut().remove(CONTENT_LENGTH);
                }

                response
            })
            .boxed();

        service
    }

    fn query_planning_service(
        &mut self,
        mut service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "query_planning_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            tracing::debug!("{} function found", FUNCTION_NAME_REQUEST);
            let this = self.clone();

            service = service
                .map_request(move |mut request: QueryPlannerRequest| {
                    let rhai_context = handle_error!(
                        this.run_rhai_script(
                            FUNCTION_NAME_REQUEST,
                            request.context.clone(),
                            request.originating_request.headers().clone()
                        ),
                        request
                    );
                    request.context = rhai_context.context;
                    *request.originating_request.headers_mut() = rhai_context.headers;
                    request
                        .originating_request
                        .headers_mut()
                        .remove(CONTENT_LENGTH);

                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "query_planning_service_response";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_RESPONSE)
        {
            tracing::debug!("{} function found", FUNCTION_NAME_RESPONSE);
            let this = self.clone();
            service = service
                .map_response(move |mut response: QueryPlannerResponse| {
                    let rhai_context = match this.run_rhai_script(
                        FUNCTION_NAME_RESPONSE,
                        response.context,
                        HeaderMap::new(),
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            let context = Context::new();
                            context
                                .insert(CONTEXT_ERROR, err)
                                .expect("error is always a string; qed");

                            return QueryPlannerResponse::builder()
                                .query_plan(response.query_plan)
                                .context(context)
                                .build();
                        }
                    };
                    response.context = rhai_context.context;

                    // Not safe to use the builders for managing responses
                    response
                })
                .boxed()
        }

        service
    }

    fn execution_service(
        &mut self,
        mut service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "execution_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            tracing::debug!("{} function found", FUNCTION_NAME_REQUEST);
            let this = self.clone();

            service = service
                .map_request(move |mut request: ExecutionRequest| {
                    let rhai_context = handle_error!(
                        this.run_rhai_script(
                            FUNCTION_NAME_REQUEST,
                            request.context.clone(),
                            request.originating_request.headers().clone()
                        ),
                        request
                    );
                    request.context = rhai_context.context;
                    *request.originating_request.headers_mut() = rhai_context.headers;
                    request
                        .originating_request
                        .headers_mut()
                        .remove(CONTENT_LENGTH);
                    // Not safe to use the builders for managing requests
                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "execution_service_response";
        service_handle_response!(self, service, FUNCTION_NAME_RESPONSE, ExecutionResponse);

        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        mut service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "subgraph_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            tracing::debug!("{} function found", FUNCTION_NAME_REQUEST);
            let this = self.clone();

            service = service
                .map_request(move |mut request: SubgraphRequest| {
                    let rhai_context = handle_error!(
                        this.run_rhai_script(
                            FUNCTION_NAME_REQUEST,
                            request.context.clone(),
                            request.subgraph_request.headers().clone()
                        ),
                        request
                    );
                    request.context = rhai_context.context;
                    *request.subgraph_request.headers_mut() = rhai_context.headers;

                    request
                        .subgraph_request
                        .headers_mut()
                        .remove(CONTENT_LENGTH);
                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "subgraph_service_response";
        let this = self.clone();
        let function_found = self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_RESPONSE);
        service = service
            .map_response(move |mut response: SubgraphResponse| {
                let previous_err: Option<String> = response
                    .context
                    .get(CONTEXT_ERROR)
                    .expect("we put the context error ourself so it will be deserializable; qed");
                if let Some(err) = previous_err {
                    return SubgraphResponse::builder()
                        .errors(vec![Error::builder()
                            .message(format!("RHAI plugin error: {}", err.as_str()))
                            .build()])
                        .context(response.context)
                        .extensions(Object::default())
                        .build();
                }
                if function_found {
                    let rhai_context = match this.run_rhai_script(
                        FUNCTION_NAME_RESPONSE,
                        response.context,
                        response.response.headers().clone(),
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            let context = Context::new();
                            context
                                .insert(CONTEXT_ERROR, err)
                                .expect("error is always a string; qed");

                            return SubgraphResponse::builder()
                                .context(context)
                                .extensions(Object::default())
                                .build();
                        }
                    };
                    response.context = rhai_context.context;
                    *response.response.headers_mut() = rhai_context.headers;
                    response.response.headers_mut().remove(CONTENT_LENGTH);
                }

                response
            })
            .boxed();

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
pub(crate) struct RhaiContext {
    headers: HeaderMap,
    context: Context,
}

impl RhaiContext {
    fn new(context: Context, headers: HeaderMap) -> Self {
        Self { context, headers }
    }

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
    fn run_rhai_script(
        &self,
        function_name: &str,
        context: Context,
        headers: HeaderMap,
    ) -> Result<RhaiContext, String> {
        let mut scope = Scope::new();
        let response: RhaiContext = self
            .engine
            .call_fn(
                &mut scope,
                &self.ast,
                function_name,
                (RhaiContext::new(context, headers),),
            )
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
            .register_indexer_set_result(Headers::set_header)
            .register_indexer_get(Headers::get_header)
            .register_indexer_set(Object::set)
            .register_indexer_get(Object::get_cloned)
            .register_indexer_set(Entries::set)
            .register_indexer_get(Entries::get_cloned)
            .register_type::<RhaiContext>()
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
