use std::sync::{Arc, Mutex};
use std::{collections::HashMap, path::PathBuf, str::FromStr};

use apollo_router_core::plugin_utils::structures;
use apollo_router_core::{
    register_plugin, Error, Object, Plugin, RouterRequest, RouterResponse, Value,
};
use futures::executor::block_on;
use http::{header::HeaderName, HeaderValue};
use rhai::serde::{from_dynamic, to_dynamic};
use rhai::{Dynamic, Engine, Scope, AST};
use serde::Deserialize;
use serde_json_bytes::ByteString;

use tower::{util::BoxService, BoxError, ServiceExt};

macro_rules! handle_error {
    ($result: expr, $message: expr, $context: expr) => {
        match $result {
            Ok(res) => res,
            Err(err) => {
                return structures::RouterResponse::builder()
                    .errors(vec![Error::builder()
                        .message(format!("RHAI plugin error. {}: {}", $message, err))
                        .build()])
                    .context($context)
                    .build()
                    .into();
            }
        }
    };
    ($result: expr, $context: expr) => {
        match $result {
            Ok(res) => res,
            Err(err) => {
                return structures::RouterResponse::builder()
                    .errors(vec![Error::builder()
                        .message(format!("RHAI plugin error: {}", err))
                        .build()])
                    .context($context)
                    .build()
                    .into();
            }
        }
    };
}

#[derive(Default, Clone)]
struct Rhai {
    ast: Option<AST>,
}

#[derive(Deserialize)]
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
struct Headers(HashMap<String, String>);
impl Headers {
    fn set_header(&mut self, name: String, value: String) {
        self.0.insert(name, value);
    }
    fn get_header(&mut self, name: String) -> String {
        self.0.get(&name).cloned().unwrap_or_default()
    }
}

impl Plugin for Rhai {
    type Config = Conf;

    fn configure(&mut self, configuration: Self::Config) -> Result<(), BoxError> {
        tracing::info!("RHAI {:#?}!", configuration.filename);
        let engine = Engine::new();
        self.ast = Some(engine.compile_file(configuration.filename)?);
        Ok(())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        let this = self.clone();
        let headers_map: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let headers_cloned = headers_map.clone();
        let service = service.map_request(move |request: RouterRequest| {
            let mut headers = headers_cloned.lock().expect("headers lock poisoned");
            *headers = request
                .context
                .request
                .headers()
                .into_iter()
                .map(|(k, v)| {
                    (
                        k.to_string(),
                        v.to_str()
                            .expect("headers are already well formatted")
                            .to_string(),
                    )
                })
                .collect();

            request
        });

        service
            .map_response(move |mut response: RouterResponse| {
                let mut engine = Engine::new();
                engine.register_indexer_set(Headers::set_header);
                engine.register_indexer_get(Headers::get_header);
                engine.register_indexer_set(Object::set);
                engine.register_indexer_get(Object::get_cloned);
                let mut scope = Scope::new();

                let extensions = block_on(async { response.context.extensions().await.clone() });
                scope.push(
                    "headers",
                    Headers(headers_map.lock().expect("headers lock poisoned").clone()),
                );
                let ext_dynamic = handle_error!(
                    to_dynamic(extensions),
                    "Cannot convert extensions to dynamic",
                    response.context
                );
                scope.push("context", ext_dynamic);

                handle_error!(
                    engine.call_fn(&mut scope, this.ast.as_ref().unwrap(), "router_service", ()),
                    response.context
                );

                // Restore headers and context from the rhai execution script
                let headers = handle_error!(
                    scope
                        .get_value::<Headers>("headers")
                        .ok_or("cannot get back headers from RHAI scope"),
                    response.context
                );

                *(block_on(async { response.context.extensions_mut().await })) = handle_error!(
                    from_dynamic(handle_error!(
                        &scope
                            .get_value::<Dynamic>("context")
                            .ok_or("cannot get back context from RHAI scope"),
                        response.context
                    )),
                    "cannot convert context from dynamic",
                    response.context
                );

                for (header_name, header_value) in &headers.0 {
                    response.response.headers_mut().append(
                        handle_error!(
                            HeaderName::from_str(header_name.as_str()),
                            format!("cannot convert '{}' to header name", header_name),
                            response.context
                        ),
                        handle_error!(
                            HeaderValue::from_str(header_value),
                            format!("cannot convert '{}' to header value", header_value),
                            response.context
                        ),
                    );
                }

                response
            })
            .boxed()
    }
}

register_plugin!("rhai", Rhai);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use apollo_router_core::{
        http_compat,
        plugin_utils::{structures, MockRouterService, RouterResponse},
        Context, DynPlugin, ResponseBody, RouterRequest,
    };
    use http::Request;
    use serde_json::Value;
    use tower::{util::BoxService, Service, ServiceExt};

    #[tokio::test]
    async fn rhai_plugin_registered() {
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
            .get("rhai")
            .expect("Plugin not found")();
        dyn_plugin
            .configure(&Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap())
            .expect("Failed to configure");
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
        let router_req = structures::RouterRequest::builder().context(context);

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
}

// Naming of methods are not really relevant or at least should be well documented (router_service, ...)
// BoxService not so easy to use, need to document .map_response and .map_request
// I found it weird to need to put HashMap for headers in a Mutex without being in an async function and not seeing any spanw function.
// Need a builder for error too

// TODO: Add other hook function (other than only router_service)
