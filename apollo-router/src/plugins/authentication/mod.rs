//! Authentication plugin

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use self::jwks::JwksManager;
use self::subgraph::SigningParams;
use self::subgraph::SigningParamsConfig;
use self::subgraph::SubgraphAuth;
use crate::configuration::connector::ConnectorConfiguration;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugins::authentication::connector::ConnectorAuth;
use crate::plugins::authentication::error::ErrorContext;
use crate::plugins::authentication::jwks::JwksConfig;
use crate::plugins::authentication::subgraph::AuthConfig;
use crate::plugins::authentication::subgraph::make_signing_params;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::connector_service::ConnectorSourceRef;
use crate::services::router;
use error::{AuthenticationError, Error};
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use http::header;
use jsonwebtoken::Algorithm;
use jsonwebtoken::decode_header;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

mod connector;
pub(crate) mod jwks;
pub(crate) mod subgraph;

mod error;
#[cfg(test)]
mod tests;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication_plugin";
pub(crate) const APOLLO_AUTHENTICATION_JWT_CLAIMS: &str = "apollo::authentication::jwt_claims";
pub(crate) const DEPRECATED_APOLLO_AUTHENTICATION_JWT_CLAIMS: &str =
    "apollo_authentication::JWT::claims";
const HEADER_TOKEN_TRUNCATED: &str = "(truncated)";

const DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL: Duration = Duration::from_secs(60);

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| Ok(Client::new()));

struct Router {
    configuration: JWTConf,
    jwks_manager: JwksManager,
}

struct AuthenticationPlugin {
    router: Option<Router>,
    subgraph: Option<SubgraphAuth>,
    connector: Option<ConnectorAuth>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Default, PartialEq)]
enum OnError {
    Continue,
    #[default]
    Error,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct JWTConf {
    /// List of JWKS used to verify tokens
    jwks: Vec<JwksConf>,
    /// HTTP header expected to contain JWT
    #[serde(default = "default_header_name")]
    header_name: String,
    /// Header value prefix
    #[serde(default = "default_header_value_prefix")]
    header_value_prefix: String,
    /// Whether to ignore any mismatched prefixes
    #[serde(default)]
    ignore_other_prefixes: bool,
    /// Alternative sources to extract the JWT
    #[serde(default)]
    sources: Vec<Source>,
    /// Control the behavior when an error occurs during the authentication process.
    ///
    /// Defaults to `Error`. When set to `Continue`, requests that fail JWT authentication will
    /// continue to be processed by the router, but without the JWT claims in the context. When set
    /// to `Error`, requests that fail JWT authentication will be rejected with a HTTP 403 error.
    #[serde(default = "OnError::default")]
    on_error: OnError,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct JwksConf {
    /// Retrieve the JWK Set
    url: String,
    /// Polling interval for each JWKS endpoint in human-readable format; defaults to 60s
    #[serde(
        deserialize_with = "humantime_serde::deserialize",
        default = "default_poll_interval"
    )]
    #[schemars(with = "String", default = "default_poll_interval")]
    poll_interval: Duration,
    /// Expected issuer for tokens verified by that JWKS
    issuer: Option<String>,
    /// List of accepted algorithms. Possible values are `HS256`, `HS384`, `HS512`, `ES256`, `ES384`, `RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`, `EdDSA`
    #[schemars(with = "Option<Vec<String>>", default)]
    #[serde(default)]
    algorithms: Option<Vec<Algorithm>>,
    /// List of headers to add to the JWKS request
    #[serde(default)]
    headers: Vec<Header>,
}

#[derive(Clone, Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
/// Insert a header
struct Header {
    /// The name of the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_name")]
    name: HeaderName,

    /// The value for the header
    #[schemars(with = "String")]
    #[serde(deserialize_with = "deserialize_header_value")]
    value: HeaderValue,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase", tag = "type")]
enum Source {
    Header {
        /// HTTP header expected to contain JWT
        #[serde(default = "default_header_name")]
        name: String,
        /// Header value prefix
        #[serde(default = "default_header_value_prefix")]
        value_prefix: String,
    },
    Cookie {
        /// Name of the cookie containing the JWT
        name: String,
    },
}

/// Authentication
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Conf {
    /// Router configuration
    router: Option<RouterConf>,
    /// Subgraph configuration
    subgraph: Option<subgraph::Config>,
    /// Connector configuration
    connector: Option<ConnectorConfiguration<AuthConfig>>,
}

// We may support additional authentication mechanisms in future, so all
// configuration (which is currently JWT specific) is isolated to the
// JWTConf structure.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RouterConf {
    /// The JWT configuration
    jwt: JWTConf,
}

fn default_header_name() -> String {
    header::AUTHORIZATION.to_string()
}

fn default_header_value_prefix() -> String {
    "Bearer".to_string()
}

fn default_poll_interval() -> Duration {
    DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL
}

#[async_trait::async_trait]
impl PluginPrivate for AuthenticationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let subgraph = Self::init_subgraph(&init).await?;
        let router = Self::init_router(&init).await?;
        let connector = Self::init_connector(init).await?;

        Ok(Self {
            router,
            subgraph,
            connector,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        // Return without layering if no router config was defined
        let Some(router_config) = &self.router else {
            return service;
        };

        fn authentication_service_span() -> impl Fn(&router::Request) -> tracing::Span + Clone {
            move |_request: &router::Request| {
                tracing::info_span!(
                    AUTHENTICATION_SPAN_NAME,
                    "authentication service" = stringify!(router::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        let jwks_manager = router_config.jwks_manager.clone();
        let configuration = router_config.configuration.clone();

        ServiceBuilder::new()
            .instrument(authentication_service_span())
            .checkpoint(move |request: router::Request| {
                Ok(authenticate(&configuration, &jwks_manager, request))
            })
            .service(service)
            .boxed()
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: crate::services::subgraph::BoxService,
    ) -> crate::services::subgraph::BoxService {
        // Return without layering if no subgraph config was defined
        let Some(subgraph) = &self.subgraph else {
            return service;
        };

        subgraph.subgraph_service(name, service)
    }

    fn connector_request_service(
        &self,
        service: crate::services::connector::request_service::BoxService,
        _: String,
    ) -> crate::services::connector::request_service::BoxService {
        // Return without layering if no connector config was defined
        let Some(connector_auth) = &self.connector else {
            return service;
        };

        connector_auth.connector_request_service(service)
    }
}

impl AuthenticationPlugin {
    async fn init_subgraph(init: &PluginInit<Conf>) -> Result<Option<SubgraphAuth>, BoxError> {
        // if no subgraph config was defined, then return early
        let Some(subgraph_conf) = init.config.subgraph.clone() else {
            return Ok(None);
        };

        let all = if let Some(config) = &subgraph_conf.all {
            Some(Arc::new(make_signing_params(config, "all").await?))
        } else {
            None
        };

        let mut subgraphs: HashMap<String, Arc<SigningParamsConfig>> = Default::default();
        for (subgraph_name, config) in &subgraph_conf.subgraphs {
            subgraphs.insert(
                subgraph_name.clone(),
                Arc::new(make_signing_params(config, subgraph_name.as_str()).await?),
            );
        }

        Ok(Some(SubgraphAuth {
            signing_params: Arc::new(SigningParams { all, subgraphs }),
        }))
    }

    async fn init_router(init: &PluginInit<Conf>) -> Result<Option<Router>, BoxError> {
        // if no router config was defined, then return early
        let Some(mut router_conf) = init.config.router.clone() else {
            return Ok(None);
        };

        if router_conf
            .jwt
            .header_value_prefix
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            return Err(Error::BadHeaderValuePrefix.into());
        }

        for source in &router_conf.jwt.sources {
            if let Source::Header { value_prefix, .. } = source {
                if value_prefix.as_bytes().iter().any(u8::is_ascii_whitespace) {
                    return Err(Error::BadHeaderValuePrefix.into());
                }
            }
        }

        router_conf.jwt.sources.insert(
            0,
            Source::Header {
                name: router_conf.jwt.header_name.clone(),
                value_prefix: router_conf.jwt.header_value_prefix.clone(),
            },
        );

        let mut list = vec![];
        for jwks_conf in &router_conf.jwt.jwks {
            let url: Url = Url::from_str(jwks_conf.url.as_str())?;
            list.push(JwksConfig {
                url,
                issuer: jwks_conf.issuer.clone(),
                algorithms: jwks_conf
                    .algorithms
                    .as_ref()
                    .map(|algs| algs.iter().cloned().collect()),
                poll_interval: jwks_conf.poll_interval,
                headers: jwks_conf.headers.clone(),
            });
        }

        tracing::info!(jwks=?router_conf.jwt.jwks, "JWT authentication using JWKSets from");

        let jwks_manager = JwksManager::new(list).await?;

        Ok(Some(Router {
            configuration: router_conf.jwt,
            jwks_manager,
        }))
    }

    async fn init_connector(init: PluginInit<Conf>) -> Result<Option<ConnectorAuth>, BoxError> {
        // if no connector config was defined, then return early
        let Some(connector_conf) = init.config.connector.clone() else {
            return Ok(None);
        };

        let mut signing_params: HashMap<ConnectorSourceRef, Arc<SigningParamsConfig>> =
            Default::default();
        for (s, source_config) in connector_conf.sources {
            let source_ref: ConnectorSourceRef = s.parse()?;
            signing_params.insert(
                source_ref.clone(),
                make_signing_params(&source_config, &source_ref.subgraph_name)
                    .await
                    .map(Arc::new)?,
            );
        }

        Ok(Some(ConnectorAuth {
            signing_params: Arc::new(signing_params),
        }))
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum JwtStatus {
    Failure {
        r#type: String,
        name: String,
        error: ErrorContext,
    },
    Success {
        r#type: String,
        name: String,
    },
}

impl JwtStatus {
    fn new_failure(header_name: impl Into<String>, error_context: ErrorContext) -> Self {
        Self::Failure {
            r#type: "header".into(),
            name: header_name.into(),
            error: error_context,
        }
    }

    fn new_success(cookie_name: impl Into<String>) -> Self {
        Self::Success {
            r#type: "cookie".into(),
            name: cookie_name.into(),
        }
    }

    /// Returns the error context if the status is a failure; Otherwise, returns None.
    fn error(&self) -> Option<&ErrorContext> {
        match self {
            Self::Failure { error, .. } => Some(error),
            _ => None,
        }
    }
}

const JWT_CONTEXT_KEY: &str = "apollo::authentication::jwt_status";

fn authenticate(
    config: &JWTConf,
    jwks_manager: &JwksManager,
    request: router::Request,
) -> ControlFlow<router::Response, router::Request> {
    // We are going to do a lot of similar checking so let's define a local function
    // to help reduce repetition
    fn failure_message(
        request: router::Request,
        config: &JWTConf,
        error: AuthenticationError,
        status: StatusCode,
    ) -> ControlFlow<router::Response, router::Request> {
        // This is a metric and will not appear in the logs
        u64_counter!(
            "apollo.router.operations.authentication.jwt",
            "Number of requests with JWT authentication",
            1,
            authentication.jwt.failed = true
        );
        tracing::error!(message = %error, "jwt authentication failure");

        let _ = request.context.insert_json_value(
            JWT_CONTEXT_KEY,
            serde_json_bytes::json!(JwtStatus::new_failure(
                config.header_name.clone(),
                error.as_context_object()
            )),
        );

        if config.on_error == OnError::Error {
            let response = router::Response::infallible_builder()
                .error(
                    graphql::Error::builder()
                        .message(error.to_string())
                        .extension_code("AUTH_ERROR")
                        .build(),
                )
                .status_code(status)
                .header(header::CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE.clone())
                .context(request.context)
                .build();

            ControlFlow::Break(response)
        } else {
            ControlFlow::Continue(request)
        }
    }

    let mut jwt = None;
    for source in &config.sources {
        let extracted_jwt = jwks::extract_jwt(
            source,
            config.ignore_other_prefixes,
            &request.router_request.headers(),
        );

        match extracted_jwt {
            None => continue,
            Some(Ok(extracted_jwt)) => {
                jwt = Some(extracted_jwt);
                break;
            }
            Some(Err(error)) => {
                return failure_message(request, config, error, StatusCode::BAD_REQUEST);
            }
        }
    }

    let jwt = match jwt {
        Some(jwt) => jwt,
        None => return ControlFlow::Continue(request),
    };

    // Try to create a valid header to work with
    let jwt_header = match decode_header(jwt) {
        Ok(h) => h,
        Err(e) => {
            // Don't reflect the jwt on error, just reply with a fixed
            // error message.
            return failure_message(
                request,
                config,
                AuthenticationError::InvalidHeader(HEADER_TOKEN_TRUNCATED.to_owned(), e),
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // Extract our search criteria from our jwt
    let criteria = jwks::JWTCriteria {
        kid: jwt_header.kid,
        alg: jwt_header.alg,
    };

    // Search our list of JWKS to find the kid and process it
    // Note: This will search through JWKS in the order in which they are defined
    // in configuration.
    if let Some(keys) = jwks::search_jwks(jwks_manager, &criteria) {
        let (issuer, token_data) = match jwks::decode_jwt(jwt, keys, criteria) {
            Ok(data) => data,
            Err((auth_error, status_code)) => {
                return failure_message(request, config, auth_error, status_code);
            }
        };

        if let Some(configured_issuer) = issuer {
            if let Some(token_issuer) = token_data
                .claims
                .as_object()
                .and_then(|o| o.get("iss"))
                .and_then(|value| value.as_str())
            {
                if configured_issuer != token_issuer {
                    return failure_message(
                        request,
                        config,
                        AuthenticationError::InvalidIssuer {
                            expected: configured_issuer,
                            token: token_issuer.to_string(),
                        },
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            }
        }

        if let Err(e) = request
            .context
            .insert(APOLLO_AUTHENTICATION_JWT_CLAIMS, token_data.claims.clone())
        {
            return failure_message(
                request,
                config,
                AuthenticationError::CannotInsertClaimsIntoContext(e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
        // This is a metric and will not appear in the logs
        u64_counter!(
            "apollo.router.operations.jwt",
            "Number of requests with JWT authentication",
            1
        );

        let _ = request.context.insert_json_value(
            JWT_CONTEXT_KEY,
            serde_json_bytes::json!(JwtStatus::new_success(
                // TODO how do I actually set this correctly?
                config.header_name.clone()
            )),
        );

        return ControlFlow::Continue(request);
    }

    // We can't find a key to process this JWT.
    let err = criteria.kid.map_or_else(
        || AuthenticationError::CannotFindSuitableKey(criteria.alg, None),
        |kid| AuthenticationError::CannotFindKID(kid),
    );

    failure_message(request, config, err, StatusCode::UNAUTHORIZED)
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_private_plugin!("apollo", "authentication", AuthenticationPlugin);
