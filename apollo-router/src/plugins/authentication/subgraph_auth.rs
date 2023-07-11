use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::SystemTime;

use aws_credential_types::provider::ProvideCredentials;
use aws_credential_types::Credentials;
use aws_sigv4::http_request;
use aws_sigv4::http_request::sign;
use aws_sigv4::http_request::PayloadChecksumKind;
use aws_sigv4::http_request::SignableBody;
use aws_sigv4::http_request::SignableRequest;
use aws_sigv4::http_request::SigningSettings;
use aws_types::region::Region;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::subgraph;
use crate::services::SubgraphRequest;

register_plugin!("apollo", "subgraph_authentication", SubgraphAuth);

/// todo[igni]: document before merging.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct AWSSigV4HardcodedConfig {
    /// todo[igni]: document before merging.
    access_key_id: String,
    /// todo[igni]: document before merging.
    secret_access_key: String,
    /// todo[igni]: document before merging.
    region: String,
    /// todo[igni]: document before merging.
    service: String,
}

impl ProvideCredentials for AWSSigV4HardcodedConfig {
    fn provide_credentials<'a>(
        &'a self,
    ) -> aws_credential_types::provider::future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        aws_credential_types::provider::future::ProvideCredentials::ready(Ok(Credentials::new(
            self.access_key_id.clone(),
            self.secret_access_key.clone(),
            None,
            None,
            "apollo-router",
        )))
    }
}

/// todo[igni]: document before merging.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
struct DefaultChainConfig {
    /// todo[igni]: document before merging.
    region: String,
    /// todo[igni]: document before merging.
    profile_name: Option<String>,
    /// todo[igni]: document before merging.
    service: String,
}

/// todo[igni]: document before merging.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
struct AssumeRoleProvider {
    /// todo[igni]: document before merging.
    role: String,
    /// todo[igni]: document before merging.
    session: String,
    /// todo[igni]: document before merging.
    region: String,
    /// todo[igni]: document before merging.
    service: String,
    /// todo[igni]: document before merging.
    external_id: Option<String>,
}

/// todo[igni]: document before merging.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
enum AWSSigV4Config {
    Hardcoded(AWSSigV4HardcodedConfig),
    DefaultChain(DefaultChainConfig),
    AssumeRoleProvider(AssumeRoleProvider),
}

#[derive(Clone, JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
enum AuthConfig {
    #[serde(rename = "aws_sig_v4")]
    /// todo[igni]: document before merging.
    AWSSigV4(AWSSigV4Config),
}

/// todo[igni]: document before merging.
#[derive(Clone, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Config {
    /// todo[igni]: document before merging.
    #[serde(default)]
    all: Option<AuthConfig>,
    #[serde(default)]
    /// todo[igni]: document before merging.
    subgraphs: HashMap<String, AuthConfig>,
}

struct SubgraphAuth {
    signing_params: SigningParams,
}

#[derive(Clone, Default)]
struct SigningParams {
    all: Option<SigningParamsConfig>,
    subgraphs: HashMap<String, SigningParamsConfig>,
}

#[derive(Clone)]
struct SigningParamsConfig {
    credentials_provider: Option<Arc<dyn ProvideCredentials>>,
    region: Region,
    service_name: String,
}

#[async_trait::async_trait]
impl Plugin for SubgraphAuth {
    type Config = Config;
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let all = if let Some(config) = &init.config.all {
            Some(make_signing_params(config).await)
        } else {
            None
        };

        let mut subgraphs: HashMap<String, SigningParamsConfig> = Default::default();
        for (subgraph_name, config) in &init.config.subgraphs {
            subgraphs.insert(subgraph_name.clone(), make_signing_params(config).await);
        }

        Ok(SubgraphAuth {
            signing_params: { SigningParams { all, subgraphs } },
        })
    }

    fn subgraph_service(&self, name: &str, service: subgraph::BoxService) -> subgraph::BoxService {
        if let Some(signing_params) = self.params_for_service(name) {
            ServiceBuilder::new()
            .checkpoint_async(move |mut req: SubgraphRequest| {
                let signing_params = signing_params.clone();
                async move {
                    let signing_params = signing_params.clone();
                if let Some(credentials_provider) = &signing_params.credentials_provider {
                    // TODO: DONT UNWRAP YO
                    let credentials = credentials_provider.provide_credentials().await.unwrap();
                    let settings = get_signing_settings(&signing_params);
                    let mut builder = http_request::SigningParams::builder()
                        .access_key(credentials.access_key_id())
                        .secret_key(credentials.secret_access_key())
                        .region(signing_params.region.as_ref())
                        .service_name(&signing_params.service_name)
                        .time(SystemTime::now())
                        .settings(settings);
                    builder.set_security_token(credentials.session_token());
                    let body_bytes = match serde_json::to_vec(&req.subgraph_request.body()) {
                        Ok(b) => b,
                        Err(err) => {
                            tracing::warn!(
                            "Failed to serialize GraphQL body for AWS SigV4 signing, skipping signing. Error: {}",
                            err
                        );
                            return Ok(ControlFlow::Continue(req));
                        }
                    };
                    // UnsignedPayload only applies to lattice
                    let signable_request = SignableRequest::new(
                        req.subgraph_request.method(),
                        req.subgraph_request.uri(),
                        req.subgraph_request.headers(),
                        match signing_params.service_name.as_str() {
                            "vpc-lattice-svcs" => SignableBody::UnsignedPayload,
                            _ => SignableBody::Bytes(&body_bytes),
                        },
                    );

                    let signing_params = builder.build().expect("all required fields set");

                    let (signing_instructions, _signature) = match sign(signable_request, &signing_params) {
                        Ok(output) => output,
                        Err(err) => {
                            tracing::warn!("Failed to sign GraphQL request for AWS SigV4, skipping signing. Error: {}", err);
                            return Ok(ControlFlow::Continue(req));
                        }
                    }.into_parts();
                    signing_instructions.apply_to_request(&mut req.subgraph_request);
                     Ok(ControlFlow::Continue(req))
                } else {
                    Ok(ControlFlow::Continue(req))
                }
            }
            }).buffered()
            .service(service)
            .boxed()
        } else {
            service
        }
    }
}

async fn make_signing_params(config: &AuthConfig) -> SigningParamsConfig {
    let default_chain = if let AuthConfig::AWSSigV4(AWSSigV4Config::DefaultChain(config)) = &config
    {
        let aws_config =
            aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                .region(aws_types::region::Region::new(config.region.clone()));

        let aws_config = if let Some(profile_name) = &config.profile_name {
            aws_config.profile_name(profile_name.as_str())
        } else {
            aws_config
        };

        aws_config.build().await
    } else {
        aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
            .build()
            .await
    };
    match config {
        AuthConfig::AWSSigV4(AWSSigV4Config::Hardcoded(config)) => {
            let region = aws_types::region::Region::new(config.region.clone());
            let credentials_provider = Arc::new(config.clone()) as Arc<dyn ProvideCredentials>;
            SigningParamsConfig {
                region,
                service_name: config.service.clone(),
                credentials_provider: Some(credentials_provider),
            }
        }
        AuthConfig::AWSSigV4(AWSSigV4Config::DefaultChain(config)) => {
            let region = aws_types::region::Region::new(config.region.clone());
            SigningParamsConfig {
                credentials_provider: Some(Arc::new(default_chain) as Arc<dyn ProvideCredentials>),
                region,
                service_name: config.service.clone(),
            }
        }
        AuthConfig::AWSSigV4(AWSSigV4Config::AssumeRoleProvider(config)) => {
            let region = aws_types::region::Region::new(config.region.clone());
            let rp = aws_config::sts::AssumeRoleProvider::builder(config.role.clone())
                .session_name(config.session.clone())
                .region(region.clone());
            let rp = if let Some(external_id) = &config.external_id {
                rp.external_id(external_id.as_str())
            } else {
                rp
            };

            let rp = rp.build(default_chain);

            SigningParamsConfig {
                credentials_provider: Some(Arc::new(rp) as Arc<dyn ProvideCredentials>),
                region,
                service_name: config.service.clone(),
            }
        }
    }
}

impl SubgraphAuth {
    fn params_for_service(&self, service_name: &str) -> Option<SigningParamsConfig> {
        self.signing_params
            .subgraphs
            .get(service_name)
            .cloned()
            .or_else(|| self.signing_params.all.clone())
    }
}

// struct AuthLayer {
//     signing_params_config: SigningParamsConfig,
// }

// impl AuthLayer {
//     fn new(signing_params_config: SigningParamsConfig) -> Self {
//         Self {
//             signing_params_config,
//         }
//     }
// }

// impl<S> Layer<S> for AuthLayer {
//     type Service = AuthLayerService<S>;

//     fn layer(&self, inner: S) -> Self::Service {
//         AuthLayerService {
//             inner,
//             signing_params_config: self.signing_params_config.clone(),
//         }
//     }
// }

// struct AuthLayerService<S> {
//     inner: S,
//     signing_params_config: SigningParamsConfig,
// }

// impl<S> Service<SubgraphRequest> for AuthLayerService<S>
// where
//     S: Service<SubgraphRequest> + Clone + Send + 'static,
//     S::Future: Send + 'static,
// {
//     type Response = S::Response;
//     type Error = S::Error;
//     type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

//     fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
//         self.inner.poll_ready(cx)
//     }

//     fn call(&mut self, mut req: SubgraphRequest) -> Self::Future {
//         let signing_params = self.signing_params_config.clone();
//         Box::pin(async move {

//         })
//     }
// }

/// There are three possible cases
/// https://github.com/awslabs/aws-sdk-rust/blob/9c3168dafa4fd8885ce4e1fd41cec55ce982a33c/sdk/aws-sigv4/src/http_request/sign.rs#L264C1-L271C6
fn get_signing_settings(signing_params: &SigningParamsConfig) -> SigningSettings {
    let mut settings = SigningSettings::default();
    settings.payload_checksum_kind = match signing_params.service_name.as_str() {
        "s3" | "vpc-lattice-svcs" => PayloadChecksumKind::XAmzSha256,
        _ => PayloadChecksumKind::NoHeader,
    };
    settings
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use http::header::CONTENT_LENGTH;
    use http::header::CONTENT_TYPE;
    use http::header::HOST;
    use regex::Regex;
    use tower::Service;

    use super::*;
    use crate::graphql::Request;
    use crate::plugin::test::MockSubgraphService;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::SubgraphResponse;
    use crate::Context;

    #[test]
    fn test_all_aws_sig_v4_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
          aws_sig_v4:
            hardcoded:
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
              hardcoded:
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

        let mut service = SubgraphAuth::new(
            PluginInit::fake_builder()
                .config(Config {
                    all: Some(AuthConfig::AWSSigV4(AWSSigV4Config::Hardcoded(
                        AWSSigV4HardcodedConfig {
                            access_key_id: "id".to_string(),
                            secret_access_key: "secret".to_string(),
                            region: "us-east-1".to_string(),
                            service: "lambda".to_string(),
                        },
                    ))),
                    subgraphs: Default::default(),
                })
                .build(),
        )
        .await
        .unwrap()
        .subgraph_service("test_subgraph", mock.boxed());

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
        SubgraphRequest::builder()
            .supergraph_request(Arc::new(
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
            ))
            .subgraph_request(
                http::Request::builder()
                    .header(HOST, "rhost")
                    .header(CONTENT_LENGTH, "22")
                    .header(CONTENT_TYPE, "graphql")
                    .body(Request::builder().query("query").build())
                    .expect("expecting valid request"),
            )
            .operation_kind(OperationKind::Query)
            .context(Context::new())
            .build()
    }
}
