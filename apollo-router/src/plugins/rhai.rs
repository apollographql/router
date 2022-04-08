//! Customization via Rhai.

use std::{path::PathBuf, str::FromStr, sync::Arc};

use apollo_router_core::{
    http_compat, plugin_utils, Context, ExecutionRequest, ExecutionResponse, Extensions,
    QueryPlannerRequest, QueryPlannerResponse, Request, SubgraphRequest, SubgraphResponse,
};
use apollo_router_core::{
    register_plugin, Error, Object, Plugin, RouterRequest, RouterResponse, Value,
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
                    return plugin_utils::$response_ty::builder()
                        .errors(vec![Error::builder()
                            .message(format!("RHAI plugin error: {}", err.as_str()))
                            .build()])
                        .context(response.context)
                        .build()
                        .into();
                }
                if function_found {
                    let ctx_request = response.context.request.clone();
                    response.context =
                        match this.run_rhai_script_arc($function_name, response.context) {
                            Ok(res) => res,
                            Err(err) => {
                                let ctx = Context::new().with_request(ctx_request);
                                ctx.insert(CONTEXT_ERROR, err)
                                    .expect("error is always a string; qed");

                                return plugin_utils::$response_ty::builder()
                                    .context(ctx)
                                    .build()
                                    .into();
                            }
                        };

                    for (header_name, header_value) in response.context.request.headers() {
                        response
                            .response
                            .headers_mut()
                            .insert(header_name, header_value.clone());
                    }
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

impl Plugin for Rhai {
    type Config = Conf;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
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
                    request.context = handle_error!(
                        this.run_rhai_script(FUNCTION_NAME_REQUEST, request.context.clone()),
                        request
                    );

                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "router_service_response";
        service_handle_response!(self, service, FUNCTION_NAME_RESPONSE, RouterResponse);

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
                    request.context = handle_error!(
                        this.run_rhai_script_arc(FUNCTION_NAME_REQUEST, request.context.clone()),
                        request
                    );

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
                    let http_request = response.context.request.clone();
                    response.context =
                        match this.run_rhai_script_arc(FUNCTION_NAME_RESPONSE, response.context) {
                            Ok(res) => res,
                            Err(err) => {
                                let ctx = Context::new().with_request(http_request);
                                ctx.insert(CONTEXT_ERROR, err)
                                    .expect("error is always a string; qed");

                                return plugin_utils::QueryPlannerResponse::builder()
                                    .context(ctx)
                                    .build()
                                    .into();
                            }
                        };

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
                    request.context = handle_error!(
                        this.run_rhai_script_arc(FUNCTION_NAME_REQUEST, request.context.clone()),
                        request
                    );
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
                    request.context = handle_error!(
                        this.run_rhai_script_arc(FUNCTION_NAME_REQUEST, request.context.clone()),
                        request
                    );

                    for (header_name, header_value) in request.context.request.headers() {
                        request
                            .http_request
                            .headers_mut()
                            .insert(header_name.clone(), header_value.clone());
                    }
                    request.http_request.headers_mut().remove(CONTENT_LENGTH);
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
                    return plugin_utils::SubgraphResponse::builder()
                        .errors(vec![Error::builder()
                            .message(format!("RHAI plugin error: {}", err.as_str()))
                            .build()])
                        .context(response.context)
                        .build()
                        .into();
                }
                if function_found {
                    let ctx_request = response.context.request.clone();
                    response.context =
                        match this.run_rhai_script_arc(FUNCTION_NAME_RESPONSE, response.context) {
                            Ok(res) => res,
                            Err(err) => {
                                let ctx = Context::new().with_request(ctx_request);
                                ctx.insert(CONTEXT_ERROR, err)
                                    .expect("error is always a string; qed");

                                return plugin_utils::SubgraphResponse::builder()
                                    .context(ctx)
                                    .build()
                                    .into();
                            }
                        };

                    for (header_name, header_value) in response.context.request.headers() {
                        response
                            .response
                            .headers_mut()
                            .insert(header_name, header_value.clone());
                    }
                    response.response.headers_mut().remove(CONTENT_LENGTH);
                }

                response
            })
            .boxed();

        service
    }
}

impl RhaiObjectSetterGetter for Extensions {
    fn set(&mut self, key: String, value: Value) {
        self.insert(key, value);
    }
    fn get_cloned(&mut self, key: String) -> Value {
        self.get(&key).map(|v| v.clone()).unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
struct RhaiContext {
    context: Context<http_compat::Request<Request>>,
}

impl RhaiContext {
    fn new(context: Context<http_compat::Request<Request>>) -> Self {
        Self { context }
    }
    fn get_headers(&mut self) -> Headers {
        Headers(self.context.request.headers().clone())
    }
    fn set_headers(&mut self, headers: Headers) {
        *self.context.request.headers_mut() = headers.0;
    }
    fn get_extensions(&mut self) -> Dynamic {
        to_dynamic(self.context.extensions.clone()).unwrap()
    }
    fn set_extensions(&mut self, extensions: Dynamic) {
        self.context.extensions = from_dynamic(&extensions).unwrap();
    }
}

impl Rhai {
    fn run_rhai_script(
        &self,
        function_name: &str,
        context: Context<http_compat::Request<Request>>,
    ) -> Result<Context<http_compat::Request<Request>>, String> {
        let mut scope = Scope::new();
        let response: RhaiContext = self
            .engine
            .call_fn(
                &mut scope,
                &self.ast,
                function_name,
                (RhaiContext::new(context),),
            )
            .map_err(|err| err.to_string())?;

        Ok(response.context)
    }

    fn run_rhai_script_arc(
        &self,
        function_name: &str,
        context: Context<Arc<http_compat::Request<Request>>>,
    ) -> Result<Context<Arc<http_compat::Request<Request>>>, String> {
        let mut scope = Scope::new();

        let mut new_context = Context::new().with_request((*context.request).clone());
        new_context.extensions = context.extensions;
        let response: RhaiContext = self
            .engine
            .call_fn(
                &mut scope,
                &self.ast,
                function_name,
                (RhaiContext::new(new_context),),
            )
            .map_err(|err| err.to_string())?;

        Ok(response.context.into())
    }

    fn new_rhai_engine() -> Engine {
        let mut engine = Engine::new();
        engine
            .set_max_expr_depths(0, 0)
            .register_indexer_set_result(Headers::set_header)
            .register_indexer_get(Headers::get_header)
            .register_indexer_set(Object::set)
            .register_indexer_get(Object::get_cloned)
            .register_indexer_set(Extensions::set)
            .register_indexer_get(Extensions::get_cloned)
            .register_type::<RhaiContext>()
            .register_get_set(
                "headers",
                RhaiContext::get_headers,
                RhaiContext::set_headers,
            )
            .register_get_set(
                "extensions",
                RhaiContext::get_extensions,
                RhaiContext::set_extensions,
            );

        engine
    }
}

register_plugin!("experimental", "rhai", Rhai);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::sync::Arc;

    use apollo_router_core::{
        http_compat::RequestBuilder,
        plugin_utils::{MockExecutionService, MockRouterService, RouterResponse},
        Context, DynPlugin, ResponseBody, RouterRequest,
    };
    use http::{Method, Uri};
    use serde_json::Value;
    use tower::{util::BoxService, Service, ServiceExt};

    #[tokio::test]
    async fn rhai_plugin_router_service() {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: RouterRequest| {
                Ok(RouterResponse::builder()
                    .context(req.context.into())
                    .build()
                    .into())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("experimental.rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap(),
            )
            .unwrap();
        let mut router_service = dyn_plugin.router_service(BoxService::new(mock_service.build()));
        let fake_req = RequestBuilder::new(Method::GET, Uri::from_str("http://test").unwrap())
            .header("X-CUSTOM-HEADER", "CUSTOM_VALUE")
            .body(
                apollo_router_core::Request::builder()
                    .query(String::new())
                    .build(),
            )
            .unwrap();
        let context = Context::new().with_request(fake_req);
        context.insert("test", 5i64).unwrap();
        let router_req = plugin_utils::RouterRequest::builder().context(context);

        let router_resp = router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().into())
            .await
            .unwrap();
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
        assert_eq!(headers.get("coming_from_extensions").unwrap(), &"value_15");
        assert_eq!(context.get::<_, i64>("test").unwrap().unwrap(), 42i64);
        assert_eq!(
            context.get::<_, String>("addition").unwrap().unwrap(),
            "Here is a new element in the context".to_string()
        );
    }

    #[tokio::test]
    async fn rhai_plugin_execution_service_error() {
        let mut mock_service = MockExecutionService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(move |req: ExecutionRequest| {
                Ok(plugin_utils::ExecutionResponse::builder()
                    .context(req.context)
                    .build()
                    .into())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("experimental.rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap(),
            )
            .unwrap();
        let mut router_service =
            dyn_plugin.execution_service(BoxService::new(mock_service.build()));
        let fake_req = RequestBuilder::new(Method::GET, Uri::from_str("http://test").unwrap())
            .header("X-CUSTOM-HEADER", "CUSTOM_VALUE")
            .body(
                apollo_router_core::Request::builder()
                    .query(String::new())
                    .build(),
            )
            .unwrap();
        let context = Context::new().with_request(Arc::new(fake_req));
        context.insert("test", 5i64).unwrap();
        let exec_req = plugin_utils::ExecutionRequest::builder().context(context);

        let exec_resp = router_service
            .ready()
            .await
            .unwrap()
            .call(exec_req.build().into())
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
    }
}
