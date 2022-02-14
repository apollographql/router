use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::mpsc::sync_channel};

use apollo_router_core::{
    register_plugin, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt,
};
use http::{header::HeaderName, HeaderMap, HeaderValue, Method, Uri};
use reqwest::Url;
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
                let req_val = String::new();
                let mut scope = Scope::new();
                scope.push("request_val", req_val);

                scope.push("headers", Headers(headers_map.lock().unwrap().clone()));
                // let service_builder = ServiceBuilder::new();
                let _: () = engine
                    .call_fn(&mut scope, this.ast.as_ref().unwrap(), "router_service", ())
                    .unwrap();
                let headers = scope.get_value::<Headers>("headers").unwrap();
                for (header_name, header_value) in &headers.0 {
                    response.response.headers_mut().append(
                        HeaderName::from_str(header_name.as_str()).unwrap(),
                        HeaderValue::from_str(header_value).unwrap(),
                    );
                }
                response.response.headers_mut().append(
                    "XTEST",
                    HeaderValue::from_str(
                        scope.get_value::<String>("request_val").as_ref().unwrap(),
                    )
                    .unwrap(),
                );

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
        Context, DynPlugin, RouterRequest, ServiceBuilderExt,
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
            .returning(move |mut req: RouterRequest| {
                req.http_request.headers_mut().append(
                    "X-Custom-Header",
                    HeaderValue::from_str("MY_CUSTOM_VALUE").unwrap(),
                );
                let resp = RouterResponse::builder();
                // resp.insert_header("XTEST", HeaderValue::from_str("hereisatest"));

                Ok(resp.build().into())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("rhai")
            .expect("Plugin not found")();
        dyn_plugin
            .configure(&Value::from_str(r#"{"filename":"test.rhai"}"#).unwrap())
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
        assert_eq!(
            router_resp.response.headers().get("XTEST").unwrap(),
            &"MYTESTINRHAISCRIPT"
        );
        assert_eq!(
            router_resp.response.headers().get("coucou").unwrap(),
            &"hello"
        );
    }
}

// Naming of methods are not relevant
// BoxService not so easy to use
