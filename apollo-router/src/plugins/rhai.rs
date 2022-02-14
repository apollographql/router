use std::{path::PathBuf, str::FromStr};

use apollo_router_core::{
    register_plugin, Plugin, RouterRequest, RouterResponse, ServiceBuilderExt,
};
use http::{header::HeaderName, HeaderMap, HeaderValue, Method, Uri};
use reqwest::Url;
use rhai::{Engine, Scope, AST};
use serde::Deserialize;
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

#[derive(Default, Clone)]
struct Rhai {
    filename: PathBuf,
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

impl Plugin for Rhai {
    type Config = Conf;

    fn configure(&mut self, configuration: Self::Config) -> Result<(), BoxError> {
        tracing::info!("RHAI {:#?}!", configuration.filename);
        self.filename = configuration.filename.clone();
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
        // service.map_request(|request: RouterRequest| {
        //     req = request.http_request.clone();
        //     request
        // });

        let resp = service
            .map_response(move |mut response: RouterResponse| {
                let engine = Engine::new();
                let req_val = String::new();
                let mut scope = Scope::new();
                scope.push("request_val", req_val);
                // let service_builder = ServiceBuilder::new();
                let _: () = engine
                    .call_fn(&mut scope, this.ast.as_ref().unwrap(), "router_service", ())
                    .unwrap();

                response.response.headers_mut().append(
                    "XTEST",
                    HeaderValue::from_str(
                        scope.get_value::<String>("request_val").as_ref().unwrap(),
                    )
                    .unwrap(),
                );

                response
            })
            .boxed();

        resp
    }
}

register_plugin!("rhai", Rhai);

#[cfg(test)]
mod tests {
    use apollo_router_core::{
        plugin_utils::{
            structures::{self, RouterRequestBuilder, RouterResponseBuilder},
            MockRouterService, RouterResponse,
        },
        DynPlugin, RouterRequest, ServiceBuilderExt,
    };
    use http::HeaderValue;
    use serde_json::Value;
    use std::str::FromStr;
    use tower::{util::BoxService, Service, ServiceExt};

    #[tokio::test]
    async fn rhai_plugin_registered() {
        let mut mock_service = MockRouterService::new();
        mock_service
            .expect_call()
            // .times(1)
            .returning(move |_req: RouterRequest| {
                let resp = RouterResponse::builder();
                // resp.insert_header("XTEST", HeaderValue::from_str("hereisatest"));

                Ok(resp.build().into())
            });

        let mut dyn_plugin: Box<dyn DynPlugin> = apollo_router_core::plugins()
            .get("rhai")
            .expect("Plugin not found")();
        dyn_plugin
            .configure(&Value::from_str("{\"filename\":\"test.rhai\"}").unwrap())
            .expect("Failed to configure");
        let resp = dyn_plugin.router_service(BoxService::new(mock_service.build()));

        let router_req = structures::RouterRequest::builder();
        resp.map_response(|resp: apollo_router_core::RouterResponse| {
            assert_eq!(
                resp.response.headers().get("XTEST").unwrap(),
                &"MYTESTINRHAISCRIPT"
            );
            resp
        })
        .ready()
        .await
        .unwrap()
        .call(router_req.build().into())
        .await
        .unwrap();
    }
}

// Naming of methods are not relevant
// BoxService not so easy to use
