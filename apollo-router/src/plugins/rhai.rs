use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::mpsc::sync_channel};

use apollo_router_core::plugin_utils::structures;
use apollo_router_core::{
    register_plugin, Error, Object, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt,
};
use http::{header::HeaderName, HeaderMap, HeaderValue, Method, Uri};
use reqwest::Url;
use rhai::plugin::RhaiResult;
use rhai::{Engine, Scope, AST};
use serde::Deserialize;
use tokio::sync::oneshot;
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

#[derive(Default, Clone)]
struct Rhai {
    // filename: PathBuf,
    ast: Option<AST>,
}

#[derive(Deserialize)]
struct Conf {
    filename: PathBuf,
}

#[derive(Clone)]
struct LightRequest {
    headers: HeaderMap,
    uri: Uri,
    method: Method,
}

impl From<RouterRequest> for LightRequest {
    fn from(router_req: RouterRequest) -> Self {
        Self {
            headers: router_req.http_request.headers().clone(),
            uri: router_req.http_request.uri().clone(),
            method: router_req.http_request.method().clone(),
        }
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
        // self.filename = configuration.filename.clone();
        let engine = Engine::new();
        self.ast = Some(engine.compile_file(configuration.filename)?);
        Ok(())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // let mut req;
        let this = self.clone();
        let headers_map: HashMap<String, String> = HashMap::new();
        let headers_map = Arc::new(Mutex::new(headers_map));
        let headers_cloned = headers_map.clone();
        let service = service.map_request(move |request: RouterRequest| {
            let mut headers = headers_cloned.lock().unwrap();
            *headers = request
                .http_request
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
                let mut scope = Scope::new();

                scope.push("headers", Headers(headers_map.lock().unwrap().clone()));

                let func_result: RhaiResult =
                    engine.call_fn(&mut scope, this.ast.as_ref().unwrap(), "router_service", ());
                if let Err(func_error) = func_result {
                    return structures::RouterResponse::builder()
                        .errors(vec![Error {
                            message: format!("RHAI plugin error: {}", func_error),
                            locations: Vec::new(),
                            path: Option::default(),
                            extensions: Object::new(),
                        }])
                        .context(response.context)
                        .build()
                        .into();
                }
                let headers = scope.get_value::<Headers>("headers").unwrap();
                for (header_name, header_value) in &headers.0 {
                    response.response.headers_mut().append(
                        HeaderName::from_str(header_name.as_str()).unwrap(),
                        HeaderValue::from_str(header_value).unwrap(),
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
    use apollo_router_core::{
        plugin_utils::{
            structures::{self, RouterRequestBuilder, RouterResponseBuilder},
            MockRouterService, RouterResponse,
        },
        Context, DynPlugin, ResponseBody, RouterRequest, ServiceBuilderExt,
    };
    use http::{HeaderValue, Request};
    use serde_json::Value;
    use std::str::FromStr;
    use tower::{util::BoxService, Service, ServiceExt};

    #[tokio::test]
    async fn rhai_plugin_registered() {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            .times(1)
            .returning(|_req: RouterRequest| {
                let resp = RouterResponse::builder();
                // resp.insert_header("XTEST", HeaderValue::from_str("hereisatest"));

                Ok(resp.build().into())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("rhai")
            .expect("Plugin not found")();
        dyn_plugin
            .configure(&Value::from_str(r#"{"filename":"tests/fixtures/test.rhai"}"#).unwrap())
            .expect("Failed to configure");
        let mut router_service = dyn_plugin.router_service(BoxService::new(mock_service.build()));

        let fake_req = Arc::new(
            Request::builder()
                .header("X-Custom-Header", "CUSTOM_VALUE")
                .body(())
                .unwrap(),
        );
        let context = Context::new().with_request(fake_req);
        let router_req = structures::RouterRequest::builder();
        let router_resp = router_service
            .ready()
            .await
            .unwrap()
            .call(router_req.build().into())
            .await
            .unwrap();
        assert_eq!(router_resp.response.status(), 200);
        let headers = router_resp.response.headers().clone();
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
    }
}

// Naming of methods are not relevant
// BoxService not so easy to use
// Builder for error too
