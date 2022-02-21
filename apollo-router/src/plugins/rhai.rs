use std::sync::{Arc, Mutex};
use std::{path::PathBuf, str::FromStr};

use apollo_router_core::{
    plugin_utils, ExecutionRequest, ExecutionResponse, QueryPlannerRequest, QueryPlannerResponse,
    SubgraphRequest, SubgraphResponse,
};
use apollo_router_core::{
    register_plugin, Error, Object, Plugin, RouterRequest, RouterResponse, Value,
};
use futures::executor::block_on;
use http::HeaderMap;
use http::{header::HeaderName, HeaderValue};
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Dynamic, Engine, EvalAltResult, Scope, AST};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json_bytes::ByteString;

use tower::{util::BoxService, BoxError, ServiceExt};

const HEADERS_VAR_NAME: &str = "headers";
const CONTEXT_VAR_NAME: &str = "context";

#[derive(Default, Clone)]
struct Rhai {
    ast: AST,
}

#[derive(Deserialize, JsonSchema)]
struct Conf {
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

#[derive(Clone)]
struct Headers(HeaderMap);
impl Headers {
    fn set_header(&mut self, name: String, value: String) -> Result<(), Box<EvalAltResult>> {
        self.0.append(
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
            .map(|h| Some(h.to_str().ok()?.to_string()))
            .flatten()
            .unwrap_or_default()
    }
}

impl Plugin for Rhai {
    type Config = Conf;

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::info!("RHAI {:#?}!", configuration.filename);
        let engine = Engine::new();
        let ast = engine.compile_file(configuration.filename)?;
        Ok(Self { ast })
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        const FUNCTION_NAME: &str = "router_service";
        if !self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME)
        {
            tracing::debug!("RHAI plugin: no router_service function found");
            return service;
        }
        let this = self.clone();
        let headers_map = Arc::new(Mutex::new(HeaderMap::new()));
        let headers_cloned = headers_map.clone();
        let service = service.map_request(move |request: RouterRequest| {
            let mut headers = headers_cloned.lock().expect("headers lock poisoned");
            *headers = request.context.request.headers().clone();

            request
        });

        service
            .map_response(move |mut response: RouterResponse| {
                let extensions = block_on(async { response.context.extensions().await.clone() });
                let (headers, context) =
                    match this.run_rhai_script(FUNCTION_NAME, headers_map, extensions) {
                        Ok(res) => res,
                        Err(err) => {
                            return plugin_utils::RouterResponse::builder()
                                .errors(vec![Error::builder()
                                    .message(format!("RHAI plugin error: {}", err))
                                    .build()])
                                .context(response.context)
                                .build()
                                .into();
                        }
                    };
                *(block_on(async { response.context.extensions_mut().await })) = context;

                for (header_name, header_value) in &headers {
                    response
                        .response
                        .headers_mut()
                        .append(header_name, header_value.clone());
                }

                response
            })
            .boxed()
    }

    fn query_planning_service(
        &mut self,
        service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        const FUNCTION_NAME: &str = "query_planning_service";
        if !self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME)
        {
            tracing::debug!("RHAI plugin: no query_planning_service function found");
            return service;
        }

        let this = self.clone();
        let headers_map = Arc::new(Mutex::new(HeaderMap::new()));
        let headers_cloned = headers_map.clone();
        let service = service.map_request(move |request: QueryPlannerRequest| {
            let mut headers = headers_cloned.lock().expect("headers lock poisoned");
            *headers = request.context.request.headers().clone();

            request
        });

        service
            .map_response(move |response: QueryPlannerResponse| {
                let extensions = block_on(async { response.context.extensions().await.clone() });
                let (_headers, context) =
                    match this.run_rhai_script(FUNCTION_NAME, headers_map, extensions) {
                        Ok(res) => res,
                        Err(err) => {
                            // there is no way to return an error properly
                            (block_on(async { response.context.extensions_mut().await }))
                                .insert("query_plan_error", err.into());
                            return plugin_utils::QueryPlannerResponse::builder()
                                .context(response.context)
                                .build()
                                .into();
                        }
                    };
                *(block_on(async { response.context.extensions_mut().await })) = context;

                response
            })
            .boxed()
    }

    fn execution_service(
        &mut self,
        service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        const FUNCTION_NAME: &str = "execution_service";
        if !self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME)
        {
            tracing::debug!("RHAI plugin: no execution_service function found");
            return service;
        }
        let this = self.clone();
        let headers_map = Arc::new(Mutex::new(HeaderMap::new()));
        let headers_cloned = headers_map.clone();
        let service = service.map_request(move |request: ExecutionRequest| {
            let mut headers = headers_cloned.lock().expect("headers lock poisoned");
            *headers = request.context.request.headers().clone();

            request
        });

        service
            .map_response(move |mut response: ExecutionResponse| {
                let previous_err = (block_on(async { response.context.extensions().await }))
                    .get("query_plan_error")
                    .cloned();
                if let Some(err) = previous_err {
                    return plugin_utils::ExecutionResponse::builder()
                        .errors(vec![Error::builder()
                            .message(format!("RHAI plugin error: {:?}", err))
                            .build()])
                        .context(response.context)
                        .build()
                        .into();
                }

                let extensions = block_on(async { response.context.extensions().await.clone() });
                let (headers, context) =
                    match this.run_rhai_script(FUNCTION_NAME, headers_map, extensions) {
                        Ok(res) => res,
                        Err(err) => {
                            return plugin_utils::ExecutionResponse::builder()
                                .errors(vec![Error::builder()
                                    .message(format!("RHAI plugin error: {}", err))
                                    .build()])
                                .context(response.context)
                                .build()
                                .into();
                        }
                    };
                *(block_on(async { response.context.extensions_mut().await })) = context;

                for (header_name, header_value) in &headers {
                    response
                        .response
                        .headers_mut()
                        .append(header_name, header_value.clone());
                }

                response
            })
            .boxed()
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const FUNCTION_NAME: &str = "subgraph_service";
        if !self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME)
        {
            tracing::debug!("RHAI plugin: no subgraph_service function found");
            return service;
        }
        let this = self.clone();
        let headers_map = Arc::new(Mutex::new(HeaderMap::new()));
        let headers_cloned = headers_map.clone();
        let service = service.map_request(move |request: SubgraphRequest| {
            let mut headers = headers_cloned.lock().expect("headers lock poisoned");
            *headers = request.context.request.headers().clone();

            request
        });

        service
            .map_response(move |mut response: SubgraphResponse| {
                let extensions = block_on(async { response.context.extensions().await.clone() });
                let (headers, context) =
                    match this.run_rhai_script(FUNCTION_NAME, headers_map, extensions) {
                        Ok(res) => res,
                        Err(err) => {
                            return plugin_utils::SubgraphResponse::builder()
                                .errors(vec![Error::builder()
                                    .message(format!("RHAI plugin error: {}", err))
                                    .build()])
                                .context(response.context)
                                .build()
                                .into();
                        }
                    };
                *(block_on(async { response.context.extensions_mut().await })) = context;

                for (header_name, header_value) in &headers {
                    response
                        .response
                        .headers_mut()
                        .append(header_name, header_value.clone());
                }

                response
            })
            .boxed()
    }
}

impl Rhai {
    fn run_rhai_script(
        &self,
        function_name: &str,
        headers_map: Arc<Mutex<HeaderMap>>,
        extensions: Object,
    ) -> Result<(HeaderMap, Object), String> {
        let mut engine = Engine::new();
        engine.register_indexer_set_result(Headers::set_header);
        engine.register_indexer_get(Headers::get_header);
        engine.register_indexer_set(Object::set);
        engine.register_indexer_get(Object::get_cloned);
        let mut scope = Scope::new();
        scope.push(
            HEADERS_VAR_NAME,
            Headers(headers_map.lock().expect("headers lock poisoned").clone()),
        );
        let ext_dynamic = to_dynamic(extensions)
            .map_err(|err| format!("Cannot convert extensions to dynamic: {:?}", err))?;
        scope.push(CONTEXT_VAR_NAME, ext_dynamic);

        engine
            .call_fn(&mut scope, &self.ast, function_name, ())
            .map_err(|err| format!("RHAI plugin error: {:?}", err))?;

        // Restore headers and context from the rhai execution script
        let headers = scope
            .get_value::<Headers>(HEADERS_VAR_NAME)
            .ok_or_else(|| "cannot get back headers from RHAI scope".to_string())?;
        let context: Object = from_dynamic(
            &scope
                .get_value::<Dynamic>(CONTEXT_VAR_NAME)
                .ok_or_else(|| "cannot get back context from RHAI scope".to_string())?,
        )
        .map_err(|err| {
            format!(
                "cannot convert context coming from RHAI scope into an Object: {:?}",
                err
            )
        })?;

        Ok((headers.0, context))
    }
}

register_plugin!("apollographql.com", "rhai", Rhai);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use apollo_router_core::{
        http_compat,
        plugin_utils::{MockExecutionService, MockRouterService, RouterResponse},
        Context, DynPlugin, ResponseBody, RouterRequest,
    };
    use http::Request;
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
            .get("apollographql.com_rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap(),
            )
            .unwrap();
        let mut router_service = dyn_plugin.router_service(BoxService::new(mock_service.build()));
        let fake_req = http_compat::Request::from(
            Request::builder()
                .header("X-CUSTOM-HEADER", "CUSTOM_VALUE")
                .body(
                    apollo_router_core::Request::builder()
                        .query(String::new())
                        .build(),
                )
                .unwrap(),
        );
        let context = Context::new().with_request(fake_req);
        context.extensions_mut().await.insert("test", 5i64.into());
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
            ResponseBody::RawJSON(_) | ResponseBody::RawString(_) => {
                panic!("should not be this kind of response")
            }
        }

        assert_eq!(headers.get("coucou").unwrap(), &"hello");
        assert_eq!(headers.get("coming_from_context").unwrap(), &"value_15");
        let extensions = context.extensions().await;
        assert_eq!(extensions.get("test").unwrap(), &42i64);
        assert_eq!(
            extensions.get("addition").unwrap(),
            &String::from("Here is a new element in the context")
        );
    }

    #[tokio::test]
    async fn rhai_plugin_execution_service() {
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
            .get("apollographql.com_rhai")
            .expect("Plugin not found")
            .create_instance(
                &Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap(),
            )
            .unwrap();
        let mut router_service =
            dyn_plugin.execution_service(BoxService::new(mock_service.build()));
        let fake_req = http_compat::Request::from(
            Request::builder()
                .header("X-CUSTOM-HEADER", "CUSTOM_VALUE")
                .body(
                    apollo_router_core::Request::builder()
                        .query(String::new())
                        .build(),
                )
                .unwrap(),
        );
        let context = Context::new().with_request(Arc::new(fake_req));
        context.extensions_mut().await.insert("test", 5i64.into());
        let exec_req = plugin_utils::ExecutionRequest::builder().context(context);

        let exec_resp = router_service
            .ready()
            .await
            .unwrap()
            .call(exec_req.build().into())
            .await
            .unwrap();
        assert_eq!(exec_resp.response.status(), 200);
        let headers = exec_resp.response.headers().clone();
        let context = exec_resp.context;
        // Check if it fails
        let body = exec_resp.response.into_body();
        if !body.errors.is_empty() {
            panic!(
                "Contains errors : {}",
                body.errors
                    .into_iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<String>>()
                    .join("\n")
            );
        }

        assert_eq!(headers.get("coucou").unwrap(), &"hello");
        let extensions = context.extensions().await;
        assert_eq!(extensions.get("test").unwrap(), &25i64);
        assert_eq!(
            extensions.get("addition").unwrap(),
            &String::from("Here is a new element in the context with value 42")
        );
    }
}
