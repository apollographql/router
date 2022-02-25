use std::{path::PathBuf, str::FromStr, sync::Arc};

use apollo_router_core::{
    plugin_utils, Context, ExecutionRequest, ExecutionResponse, QueryPlannerRequest,
    QueryPlannerResponse, SubgraphRequest, SubgraphResponse,
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
                let previous_err = block_on(async {
                    response
                        .context
                        .extensions()
                        .read()
                        .await
                        .get(CONTEXT_ERROR)
                        .cloned()
                });
                if let Some(err) = previous_err {
                    return plugin_utils::$response_ty::builder()
                        .errors(vec![Error::builder()
                            .message(format!(
                                "RHAI plugin error: {}",
                                err.as_str().expect("previous error must be a string")
                            ))
                            .build()])
                        .context(response.context)
                        .build()
                        .into();
                }
                if function_found {
                    let extensions =
                        block_on(async { response.context.extensions().read().await.clone() });
                    let (headers, context) = match this.run_rhai_script(
                        $function_name,
                        response.context.request.headers(),
                        extensions,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            return plugin_utils::$response_ty::builder()
                                .errors(vec![Error::builder()
                                    .message(format!("RHAI plugin error: {}", err))
                                    .build()])
                                .context(response.context)
                                .build()
                                .into();
                        }
                    };
                    block_on(async { *response.context.extensions().write().await = context });

                    for (header_name, header_value) in &headers {
                        response
                            .response
                            .headers_mut()
                            .append(header_name, header_value.clone());
                    }
                }

                response
            })
            .boxed();
    };
}

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
        tracing::info!("RHAI {:#?}!", configuration.filename);
        let engine = Engine::new();
        let ast = engine.compile_file(configuration.filename)?;
        Ok(Self { ast })
    }

    fn router_service(
        &mut self,
        mut service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "map_router_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            let this = self.clone();
            tracing::debug!("RHAI plugin: router_service_request function found");

            service = service
                .map_request(move |mut request: RouterRequest| {
                    let extensions =
                        block_on(async { request.context.extensions().read().await.clone() });
                    let (headers, extensions) = match this.run_rhai_script(
                        FUNCTION_NAME_REQUEST,
                        request.context.request.headers(),
                        extensions,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            block_on(async {
                                request
                                    .context
                                    .insert_extension(CONTEXT_ERROR, err.into())
                                    .await
                            });
                            return request;
                        }
                    };
                    block_on(async {
                        *request.context.extensions().write().await = extensions;
                    });

                    for (header_name, header_value) in &headers {
                        request
                            .context
                            .request
                            .headers_mut()
                            .append(header_name, header_value.clone());
                    }

                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "map_router_service_response";
        service_handle_response!(self, service, FUNCTION_NAME_RESPONSE, RouterResponse);

        service
    }

    fn query_planning_service(
        &mut self,
        mut service: BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError>,
    ) -> BoxService<QueryPlannerRequest, QueryPlannerResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "map_query_planning_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            tracing::debug!("RHAI plugin: {} function found", FUNCTION_NAME_REQUEST);
            let this = self.clone();

            service = service
                .map_request(move |request: QueryPlannerRequest| {
                    let extensions =
                        block_on(async { request.context.extensions().read().await.clone() });
                    let (_headers, extensions) = match this.run_rhai_script(
                        FUNCTION_NAME_REQUEST,
                        request.context.request.headers(),
                        extensions,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            block_on(async {
                                request
                                    .context
                                    .insert_extension(CONTEXT_ERROR, err.into())
                                    .await
                            });
                            return request;
                        }
                    };
                    block_on(async {
                        *request.context.extensions().write().await = extensions;
                    });

                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "map_query_planning_service_response";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_RESPONSE)
        {
            tracing::debug!("RHAI plugin: {} function found", FUNCTION_NAME_RESPONSE);
            let this = self.clone();
            service = service
                .map_response(move |response: QueryPlannerResponse| {
                    let extensions =
                        block_on(async { response.context.extensions().read().await.clone() });
                    let (headers, extensions) = match this.run_rhai_script(
                        FUNCTION_NAME_RESPONSE,
                        response.context.request.headers(),
                        extensions,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            // there is no way to return an error properly
                            block_on(async {
                                response
                                    .context
                                    .insert_extension(CONTEXT_ERROR, err.into())
                                    .await;
                            });
                            return plugin_utils::QueryPlannerResponse::builder()
                                .context(response.context)
                                .build()
                                .into();
                        }
                    };
                    let mut http_request = (&*response.context.request).clone();
                    for (header_name, header_value) in &headers {
                        http_request
                            .headers_mut()
                            .append(header_name, header_value.clone());
                    }
                    let ctx = Context::new().with_request(Arc::new(http_request));
                    block_on(async {
                        *ctx.extensions().write().await = extensions;
                    });

                    plugin_utils::QueryPlannerResponse::builder()
                        .context(ctx)
                        .build()
                        .into()
                })
                .boxed()
        }

        service
    }

    fn execution_service(
        &mut self,
        mut service: BoxService<ExecutionRequest, ExecutionResponse, BoxError>,
    ) -> BoxService<ExecutionRequest, ExecutionResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "map_execution_service_request";
        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            tracing::debug!("RHAI plugin: {} function found", FUNCTION_NAME_REQUEST);
            let this = self.clone();

            service = service
                .map_request(move |request: ExecutionRequest| {
                    let extensions =
                        block_on(async { request.context.extensions().read().await.clone() });
                    let (headers, extensions) = match this.run_rhai_script(
                        FUNCTION_NAME_REQUEST,
                        request.context.request.headers(),
                        extensions,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            block_on(async {
                                request
                                    .context
                                    .insert_extension(CONTEXT_ERROR, err.into())
                                    .await
                            });
                            return request;
                        }
                    };
                    let mut http_request = (&*request.context.request).clone();
                    for (header_name, header_value) in &headers {
                        http_request
                            .headers_mut()
                            .insert(header_name, header_value.clone());
                    }

                    let ctx = Context::new().with_request(Arc::new(http_request));
                    block_on(async {
                        *ctx.extensions().write().await = extensions;
                    });

                    plugin_utils::ExecutionRequest::builder()
                        .context(ctx)
                        .build()
                        .into()
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "map_execution_service_response";
        service_handle_response!(self, service, FUNCTION_NAME_RESPONSE, ExecutionResponse);

        service
    }

    fn subgraph_service(
        &mut self,
        _name: &str,
        mut service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        const FUNCTION_NAME_REQUEST: &str = "map_subgraph_service_request";

        if self
            .ast
            .iter_fn_def()
            .any(|fn_def| fn_def.name == FUNCTION_NAME_REQUEST)
        {
            let this = self.clone();
            tracing::debug!("RHAI plugin: {} function found", FUNCTION_NAME_REQUEST);
            service = service
                .map_request(move |mut request: SubgraphRequest| {
                    let extensions =
                        block_on(async { request.context.extensions().read().await.clone() });
                    let (headers, extensions) = match this.run_rhai_script(
                        FUNCTION_NAME_REQUEST,
                        request.context.request.headers(),
                        extensions,
                    ) {
                        Ok(res) => res,
                        Err(err) => {
                            (block_on(async {
                                request
                                    .context
                                    .insert_extension(CONTEXT_ERROR, err.into())
                                    .await
                            }));
                            return request;
                        }
                    };
                    block_on(async {
                        *request.context.extensions().write().await = extensions;
                    });

                    for (header_name, header_value) in &headers {
                        request
                            .http_request
                            .headers_mut()
                            .insert(header_name, header_value.clone());
                    }

                    request
                })
                .boxed();
        }

        const FUNCTION_NAME_RESPONSE: &str = "map_subgraph_service_response";
        service_handle_response!(self, service, FUNCTION_NAME_RESPONSE, SubgraphResponse);

        service
    }
}

#[derive(Clone, Debug)]
struct RhaiContext {
    headers: Headers,
    extensions: Dynamic,
}

impl RhaiContext {
    fn new(headers: Headers, extensions: Dynamic) -> Self {
        Self {
            headers,
            extensions,
        }
    }
    fn get_headers(&mut self) -> Headers {
        self.headers.clone()
    }
    fn set_headers(&mut self, headers: Headers) {
        self.headers = headers;
    }
    fn get_extensions(&mut self) -> Dynamic {
        self.extensions.clone()
    }
    fn set_extensions(&mut self, extensions: Dynamic) {
        self.extensions = extensions;
    }
}

impl Rhai {
    fn run_rhai_script(
        &self,
        function_name: &str,
        headers_map: &HeaderMap,
        extensions: Object,
    ) -> Result<(HeaderMap, Object), String> {
        let mut engine = Engine::new();
        engine
            .register_indexer_set_result(Headers::set_header)
            .register_indexer_get(Headers::get_header)
            .register_indexer_set(Object::set)
            .register_indexer_get(Object::get_cloned)
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
        let mut scope = Scope::new();
        let ext_dynamic = to_dynamic(extensions)
            .map_err(|err| format!("Cannot convert extensions to dynamic: {:?}", err))?;
        let response: RhaiContext = engine
            .call_fn(
                &mut scope,
                &self.ast,
                function_name,
                (RhaiContext::new(Headers(headers_map.clone()), ext_dynamic),),
            )
            .map_err(|err| err.to_string())?;

        // Restore headers and context from the rhai execution script
        let context: Object = from_dynamic(&response.extensions).map_err(|err| {
            format!(
                "cannot convert context coming from RHAI scope into an Object: {:?}",
                err
            )
        })?;

        Ok((response.headers.0, context))
    }
}

register_plugin!("apollographql.com", "rhai", Rhai);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::sync::Arc;

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
            .get("apollographql.com.rhai")
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
        context.insert_extension("test", 5i64.into()).await;
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
        assert_eq!(headers.get("coming_from_extensions").unwrap(), &"value_15");
        assert_eq!(
            context
                .get_extension("test")
                .await
                .unwrap()
                .as_i64()
                .unwrap(),
            42i64
        );
        assert_eq!(
            context
                .get_extension("addition")
                .await
                .unwrap()
                .as_str()
                .unwrap(),
            "Here is a new element in the context"
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
            .get("apollographql.com.rhai")
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
        context.insert_extension("test", 5i64.into()).await;
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
            "RHAI plugin error: Runtime error: An error occured (line 25, position 5) in call to function map_execution_service_request"
        );
    }
}
