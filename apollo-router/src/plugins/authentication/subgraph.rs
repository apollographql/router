use core::ops::ControlFlow;
use std::collections::HashMap;
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
use crate::services::SubgraphRequest;

/// Hardcoded Config using access_key and secret.
/// Prefer using DefaultChain instead.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct AWSSigV4HardcodedConfig {
    /// The ID for this access key.
    access_key_id: String,
    /// The secret key used to sign requests.
    secret_access_key: String,
    /// The AWS region this chain applies to.
    region: String,
    /// The service you're trying to access, eg: "s3", "vpc-lattice-svcs", etc.
    service_name: String,
    /// Specify assumed role configuration.
    assume_role: Option<AssumeRoleProvider>,
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

/// Configuration of the DefaultChainProvider
#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct DefaultChainConfig {
    /// The AWS region this chain applies to.
    region: String,
    /// The profile name used by this provider
    profile_name: Option<String>,
    /// The service you're trying to access, eg: "s3", "vpc-lattice-svcs", etc.
    service_name: String,
    /// Specify assumed role configuration.
    assume_role: Option<AssumeRoleProvider>,
}

/// Specify assumed role configuration.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssumeRoleProvider {
    /// Amazon Resource Name (ARN)
    /// for the role assumed when making requests
    role_arn: String,
    /// Uniquely identify a session when the same role is assumed by different principals or for different reasons.
    session_name: String,
    /// Unique identifier that might be required when you assume a role in another account.
    external_id: Option<String>,
}

/// Configure AWS sigv4 auth.
#[derive(Clone, JsonSchema, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AWSSigV4Config {
    Hardcoded(AWSSigV4HardcodedConfig),
    DefaultChain(DefaultChainConfig),
}

impl AWSSigV4Config {
    async fn get_credentials_provider(&self) -> Arc<dyn ProvideCredentials> {
        let region = self.region();

        let role_provider_builder = self.assume_role().map(|assume_role_provider| {
            let rp =
                aws_config::sts::AssumeRoleProvider::builder(assume_role_provider.role_arn.clone())
                    .session_name(assume_role_provider.session_name.clone())
                    .region(region.clone());
            if let Some(external_id) = &assume_role_provider.external_id {
                rp.external_id(external_id.as_str())
            } else {
                rp
            }
        });

        match self {
            Self::DefaultChain(config) => {
                let aws_config =
                    aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                        .region(region.clone());

                let aws_config = if let Some(profile_name) = &config.profile_name {
                    aws_config.profile_name(profile_name.as_str())
                } else {
                    aws_config
                };

                let chain = aws_config.build().await;
                if let Some(assume_role_provider) = role_provider_builder {
                    Arc::new(assume_role_provider.build(chain))
                } else {
                    Arc::new(chain)
                }
            }
            Self::Hardcoded(config) => {
                let chain =
                    aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                        .build()
                        .await;
                if let Some(assume_role_provider) = role_provider_builder {
                    Arc::new(assume_role_provider.build(chain))
                } else {
                    Arc::new(config.clone())
                }
            }
        }
    }

    fn region(&self) -> Region {
        let region = match self {
            Self::DefaultChain(config) => config.region.clone(),
            Self::Hardcoded(config) => config.region.clone(),
        };
        aws_types::region::Region::new(region)
    }

    fn service_name(&self) -> String {
        match self {
            Self::DefaultChain(config) => config.service_name.clone(),
            Self::Hardcoded(config) => config.service_name.clone(),
        }
    }

    fn assume_role(&self) -> Option<AssumeRoleProvider> {
        match self {
            Self::DefaultChain(config) => config.assume_role.clone(),
            Self::Hardcoded(config) => config.assume_role.clone(),
        }
    }
}

#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum AuthConfig {
    #[serde(rename = "aws_sig_v4")]
    AWSSigV4(AWSSigV4Config),
}

/// Configure subgraph authentication
#[derive(Clone, Debug, Default, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct Config {
    /// Configuration that will apply to all subgraphs.
    #[serde(default)]
    pub(crate) all: Option<AuthConfig>,
    #[serde(default)]
    /// Create a configuration that will apply only to a specific subgraph.
    pub(crate) subgraphs: HashMap<String, AuthConfig>,
}

#[allow(dead_code)]
#[derive(Clone, Default)]
pub(crate) struct SigningParams {
    pub(crate) all: Option<SigningParamsConfig>,
    pub(crate) subgraphs: HashMap<String, SigningParamsConfig>,
}

#[derive(Clone)]
pub(crate) struct SigningParamsConfig {
    credentials_provider: Arc<dyn ProvideCredentials>,
    region: Region,
    service_name: String,
}

fn increment_success_counter(subgraph_name: &str) {
    tracing::info!(
        monotonic_counter.apollo.router.operations.authentication.aws.sigv4 = 1u64,
        authentication.aws.sigv4.failed = false,
        subgraph.service.name = %subgraph_name,
    );
}
fn increment_failure_counter(subgraph_name: &str) {
    tracing::info!(
        monotonic_counter.apollo.router.operations.authentication.aws.sigv4 = 1u64,
        authentication.aws.sigv4.failed = true,
        subgraph.service.name = %subgraph_name,
    );
}

pub(super) async fn make_signing_params(
    config: &AuthConfig,
    subgraph_name: &str,
) -> Result<SigningParamsConfig, BoxError> {
    match config {
        AuthConfig::AWSSigV4(config) => {
            let credentials_provider = config.get_credentials_provider().await;
            if let Err(e) = credentials_provider.provide_credentials().await {
                let error_subgraph_name = if subgraph_name == "all" {
                    "all subgraphs".to_string()
                } else {
                    format!("{} subgraph", subgraph_name)
                };
                return Err(format!(
                    "auth: {}: couldn't get credentials from provider: {}",
                    error_subgraph_name, e,
                )
                .into());
            }

            Ok(SigningParamsConfig {
                region: config.region(),
                service_name: config.service_name(),
                credentials_provider,
            })
        }
    }
}

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

pub(super) struct SubgraphAuth {
    pub(super) signing_params: SigningParams,
}

impl SubgraphAuth {
    pub(super) fn subgraph_service(
        &self,
        name: &str,
        service: crate::services::subgraph::BoxService,
    ) -> crate::services::subgraph::BoxService {
        if let Some(signing_params) = self.params_for_service(name) {
            let name = name.to_string();
            ServiceBuilder::new()
                .checkpoint_async(move |mut req: SubgraphRequest| {
                    let signing_params = signing_params.clone();
                    let name = name.clone();
                    async move {
                        let credentials = signing_params
                            .credentials_provider
                            .provide_credentials()
                            .await
                            .map_err(|err| {
                                increment_failure_counter(name.as_str());
                                let error = format!(
                                    "failed to get credentials for AWS SigV4 signing: {}",
                                    err
                                );
                                tracing::error!("{}", error);
                                error
                            })?;

                        let settings = get_signing_settings(&signing_params);
                        let mut builder = http_request::SigningParams::builder()
                            .access_key(credentials.access_key_id())
                            .secret_key(credentials.secret_access_key())
                            .region(signing_params.region.as_ref())
                            .service_name(&signing_params.service_name)
                            .time(SystemTime::now())
                            .settings(settings);
                        builder.set_security_token(credentials.session_token());
                        let body_bytes =
                            serde_json::to_vec(&req.subgraph_request.body()).map_err(|err| {
                                increment_failure_counter(name.as_str());
                                let error = format!(
                                    "failed to serialize GraphQL body for AWS SigV4 signing: {}",
                                    err
                                );
                                tracing::error!("{}", error);
                                error
                            })?;

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

                        let (signing_instructions, _signature) =
                            sign(signable_request, &signing_params)
                                .map_err(|err| {
                                    increment_failure_counter(name.as_str());
                                    let error = format!(
                                        "failed to sign GraphQL body for AWS SigV4: {}",
                                        err
                                    );
                                    tracing::error!("{}", error);
                                    error
                                })?
                                .into_parts();
                        signing_instructions.apply_to_request(&mut req.subgraph_request);
                        increment_success_counter(name.as_str());
                        Ok(ControlFlow::Continue(req))
                    }
                })
                .buffered()
                .service(service)
                .boxed()
        } else {
            service
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

    async fn test_signing_settings(service_name: &str) -> SigningSettings {
        let params: SigningParamsConfig = make_signing_params(
            &AuthConfig::AWSSigV4(AWSSigV4Config::Hardcoded(AWSSigV4HardcodedConfig {
                access_key_id: "id".to_string(),
                secret_access_key: "secret".to_string(),
                region: "us-east-1".to_string(),
                service_name: service_name.to_string(),
                assume_role: None,
            })),
            "all",
        )
        .await
        .unwrap();
        get_signing_settings(&params)
    }

    #[tokio::test]
    async fn test_get_signing_settings() {
        assert_eq!(
            PayloadChecksumKind::XAmzSha256,
            test_signing_settings("s3").await.payload_checksum_kind
        );
        assert_eq!(
            PayloadChecksumKind::XAmzSha256,
            test_signing_settings("vpc-lattice-svcs")
                .await
                .payload_checksum_kind
        );
        assert_eq!(
            PayloadChecksumKind::NoHeader,
            test_signing_settings("something-else")
                .await
                .payload_checksum_kind
        );
    }

    #[test]
    fn test_all_aws_sig_v4_hardcoded_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
          aws_sig_v4:
            hardcoded:
              access_key_id: "test"
              secret_access_key: "test"
              region: "us-east-1"
              service_name: "lambda"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_subgraph_aws_sig_v4_hardcoded_config() {
        serde_yaml::from_str::<Config>(
            r#"
        subgraphs:
          products:
            aws_sig_v4:
              hardcoded:
                access_key_id: "test"
                secret_access_key: "test"
                region: "us-east-1"
                service_name: "test_service"
        "#,
        )
        .unwrap();
    }

    #[test]
    fn test_aws_sig_v4_default_chain_assume_role_config() {
        serde_yaml::from_str::<Config>(
            r#"
        all:
            aws_sig_v4:
                default_chain:
                    profile_name: "my-test-profile"
                    region: "us-east-1"
                    service_name: "lambda"
                    assume_role:
                        role_arn: "test-arn"
                        session_name: "test-session"
                        external_id: "test-id"
        "#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_lattice_body_payload_should_be_unsigned() -> Result<(), BoxError> {
        let subgraph_request = example_request();

        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                assert_eq!(
                    "UNSIGNED-PAYLOAD",
                    request
                        .subgraph_request
                        .headers()
                        .get("x-amz-content-sha256")
                        .unwrap()
                        .to_str()
                        .unwrap()
                );
                true
            })
            .returning(example_response);

        let mut service = SubgraphAuth {
            signing_params: SigningParams {
                all: make_signing_params(
                    &AuthConfig::AWSSigV4(AWSSigV4Config::Hardcoded(AWSSigV4HardcodedConfig {
                        access_key_id: "id".to_string(),
                        secret_access_key: "secret".to_string(),
                        region: "us-east-1".to_string(),
                        service_name: "vpc-lattice-svcs".to_string(),
                        assume_role: None,
                    })),
                    "all",
                )
                .await
                .ok(),
                subgraphs: Default::default(),
            },
        }
        .subgraph_service("test_subgraph", mock.boxed());

        service.ready().await?.call(subgraph_request).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_aws_sig_v4_headers() -> Result<(), BoxError> {
        let subgraph_request = example_request();

        let mut mock = MockSubgraphService::new();
        mock.expect_call()
            .times(1)
            .withf(|request| {
                let authorization_regex = Regex::new(r"AWS4-HMAC-SHA256 Credential=id/\d{8}/us-east-1/s3/aws4_request, SignedHeaders=content-length;content-type;host;x-amz-content-sha256;x-amz-date, Signature=[a-f0-9]{64}").unwrap();
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

        let mut service = SubgraphAuth {
            signing_params: SigningParams {
                all: make_signing_params(
                    &AuthConfig::AWSSigV4(AWSSigV4Config::Hardcoded(AWSSigV4HardcodedConfig {
                        access_key_id: "id".to_string(),
                        secret_access_key: "secret".to_string(),
                        region: "us-east-1".to_string(),
                        service_name: "s3".to_string(),
                        assume_role: None,
                    })),
                    "all",
                )
                .await
                .ok(),
                subgraphs: Default::default(),
            },
        }
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
