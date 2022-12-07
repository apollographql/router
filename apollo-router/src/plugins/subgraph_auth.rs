use std::collections::HashMap;
use std::task::Context;
use std::task::Poll;
use std::time::SystemTime;

use aws_sigv4::http_request::{
    sign, PayloadChecksumKind, SignableBody, SignableRequest, SigningParams, SigningSettings,
};
use aws_types::Credentials;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::Layer;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower_service::Service;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::SubgraphRequest;

register_plugin!("apollo", "subgraph_auth", SubgraphAuth);

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct AWSSigV4Config {
    access_key_id: String,
    secret_access_key: String,
    region: String,
    service: String,
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
enum AuthConfig {
    #[serde(rename = "aws_sig_v4")]
    AWSSigV4(AWSSigV4Config),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    all: Option<AuthConfig>,
    #[serde(default)]
    subgraphs: HashMap<String, AuthConfig>,
}

struct SubgraphAuth {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for SubgraphAuth {
    type Config = Config;
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(SubgraphAuth {
            config: init.config,
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        let mut auth = self.config.all.as_ref();
        if let Some(subgraph) = self.config.subgraphs.get(name) {
            auth = Some(subgraph);
        }
        match auth {
            Some(auth) => ServiceBuilder::new()
                .layer(AuthLayer::new(auth.to_owned()))
                .service(service)
                .boxed(),
            None => service,
        }
    }
}

struct AuthLayer {
    auth: AuthConfig,
}

impl AuthLayer {
    fn new(auth: AuthConfig) -> Self {
        Self { auth }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthLayerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthLayerService {
            inner,
            auth: self.auth.clone(),
        }
    }
}

struct AuthLayerService<S> {
    inner: S,
    auth: AuthConfig,
}

impl<S> Service<SubgraphRequest> for AuthLayerService<S>
where
    S: Service<SubgraphRequest>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
        match &self.auth {
            AuthConfig::AWSSigV4(config) => {
                let credentials = Credentials::new(
                    config.access_key_id.clone(),
                    config.secret_access_key.clone(),
                    None,
                    None,
                    "config",
                );

                let mut settings = SigningSettings::default();
                settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;

                let mut builder = SigningParams::builder()
                    .access_key(credentials.access_key_id())
                    .secret_key(credentials.secret_access_key())
                    .region(config.region.as_ref())
                    .service_name(config.service.as_ref())
                    .time(SystemTime::now())
                    .settings(settings);

                builder.set_security_token(credentials.session_token());
                let signing_params = builder.build().expect("all required fields set");

                let body_bytes = match serde_json::to_string(&req.subgraph_request.body()) {
                    Ok(body_str) => body_str.as_bytes().to_owned(),
                    Err(err) => {
                        tracing::error!("Failed to serialize GraphQL body for AWS SigV4 signing, skipping signing. Error: {}", err);
                        return self.inner.call(req);
                    }
                };

                let signable_request = SignableRequest::new(
                    req.subgraph_request.method(),
                    req.subgraph_request.uri(),
                    req.subgraph_request.headers(),
                    SignableBody::Bytes(&body_bytes),
                );
                let (signing_instructions, _signature) = match sign(signable_request, &signing_params) {
                    Ok(output) => output,
                    Err(err) => {
                        tracing::error!("Failed to sign GraphQL request for AWS SigV4, skipping signing. Error: {}", err);
                        return self.inner.call(req);
                    }
                }.into_parts();

                signing_instructions.apply_to_request(&mut req.subgraph_request);

                self.inner.call(req)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use super::*;
    use crate::graphql::Request;
    use crate::plugin::test::MockSubgraphService;
    use crate::plugins::subgraph_auth::AuthConfig;
    use crate::plugins::subgraph_auth::AuthLayer;
    use crate::query_planner::fetch::OperationKind;
    use crate::Context;
    use crate::SubgraphRequest;
    use crate::SubgraphResponse;

    use http::header::CONTENT_LENGTH;
    use http::header::CONTENT_TYPE;
    use http::header::HOST;

    use regex::Regex;

    #[test]
    fn test_all_aws_sig_v4_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
          aws_sig_v4:
            access_key_id: "test"
            secret_access_key: "test"
            region: "us-east-1"
            service: "lambda"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_subgraph_aws_sig_v4_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            aws_sig_v4:
              access_key_id: "test"
              secret_access_key: "test"
              region: "us-east-1"
              service: "lambda"
        "#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_aws_sig_v4_headers() -> Result<(), BoxError> {
        let subgraph_request = example_request();

        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                let authorization_regex = Regex::new(r"AWS4-HMAC-SHA256 Credential=id/\d{8}/us-east-1/lambda/aws4_request, SignedHeaders=content-length;content-type;host;x-amz-content-sha256;x-amz-date, Signature=[a-f0-9]{64}").unwrap();
                let authorization_header_str = request.subgraph_request.headers().get("authorization").unwrap().to_str().unwrap();
                assert_eq!(match authorization_regex.find(authorization_header_str) {
                    Some(m) => m.as_str(),
                    None => "no match"
                }, authorization_header_str);

                let x_amz_date_regex = Regex::new(r"\d{8}T\d{6}Z").unwrap();
                let x_amz_date_header_str = request.subgraph_request.headers().get("x-amz-date").unwrap().to_str().unwrap();
                assert_eq!(match x_amz_date_regex.find(x_amz_date_header_str) {
                    Some(m) => m.as_str(),
                    None => "no match"
                }, x_amz_date_header_str);

                assert_eq!(request.subgraph_request.headers().get("x-amz-content-sha256").unwrap(), "255959b4c6e11c1080f61ce0d75eb1b565c1772173335a7828ba9c13c25c0d8c");

                true
            })
            .returning(example_response);

        let mut service = AuthLayer::new(AuthConfig::AWSSigV4(AWSSigV4Config {
            access_key_id: "id".to_string(),
            secret_access_key: "secret".to_string(),
            region: "us-east-1".to_string(),
            service: "lambda".to_string(),
        }))
        .layer(mock);

        service.ready().await?.call(subgraph_request).await?;
        Ok(())
    }

    fn example_response(_: SubgraphRequest) -> Result<SubgraphResponse, BoxError> {
        Ok(SubgraphResponse::new_from_response(
            http::Response::default(),
            Context::new(),
        ))
    }

    fn example_request() -> SubgraphRequest {
        let ctx = Context::new();
        SubgraphRequest {
            supergraph_request: Arc::new(
                http::Request::builder()
                    .header(HOST, "host")
                    .header(CONTENT_LENGTH, "2")
                    .header(CONTENT_TYPE, "graphql")
                    .body(
                        Request::builder()
                            .query("query")
                            .operation_name("my_operation_name")
                            .build(),
                    )
                    .expect("expecting valid request"),
            ),
            subgraph_request: http::Request::builder()
                .header(HOST, "rhost")
                .header(CONTENT_LENGTH, "22")
                .header(CONTENT_TYPE, "graphql")
                .body(Request::builder().query("query").build())
                .expect("expecting valid request"),
            operation_kind: OperationKind::Query,
            context: ctx,
        }
    }
}
