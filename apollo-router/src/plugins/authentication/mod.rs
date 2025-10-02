//! Authentication plugin

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use error::AuthenticationError;
use error::Error;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use http::header;
use jsonwebtoken::Algorithm;
use jsonwebtoken::decode_header;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

use self::jwks::JwksManager;
use self::subgraph::SigningParams;
use self::subgraph::SigningParamsConfig;
use self::subgraph::SubgraphAuth;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugins::authentication::connector::ConnectorAuth;
use crate::plugins::authentication::error::ErrorContext;
use crate::plugins::authentication::jwks::Audiences;
use crate::plugins::authentication::jwks::Issuers;
use crate::plugins::authentication::jwks::JwksConfig;
use crate::plugins::authentication::subgraph::make_signing_params;
use crate::services::APPLICATION_JSON_HEADER_VALUE;
use crate::services::connector_service::ConnectorSourceRef;
use crate::services::router;

pub(crate) mod jwks;

pub(crate) mod connector;

pub(crate) mod subgraph;

mod error;
#[cfg(test)]
mod tests;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication_plugin";
pub(crate) const APOLLO_AUTHENTICATION_JWT_CLAIMS: &str = "apollo::authentication::jwt_claims";
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
enum OnError {
    Continue,
    Error,
}

impl Default for OnError {
    fn default() -> Self {
        Self::Error
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, serde_derive_default::Default)]
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
    #[serde(default)]
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
    /// Expected issuers for tokens verified by that JWKS
    ///
    /// If not specified, the issuer will not be checked.
    issuers: Option<Issuers>,
    /// Expected audiences for tokens verified by that JWKS
    ///
    /// If not specified, the audience will not be checked.
    audiences: Option<Audiences>,
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

impl Source {
    fn as_textual_representation(&self) -> String {
        match self {
            Source::Header { name, .. } => format!("header:{}", name),
            Source::Cookie { name } => format!("cookie:{}", name),
        }
    }
}

/// Authentication
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "AuthenticationConfig")]
struct Conf {
    /// Router configuration
    router: Option<RouterConf>,
    /// Subgraph configuration
    subgraph: Option<subgraph::Config>,
    /// Connector configuration
    connector: Option<connector::Config>,
}

// We may support additional authentication mechanisms in future, so all
// configuration (which is currently JWT specific) is isolated to the
// JWTConf structure.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "AuthenticationRouterConfig")]
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
                    "otel.kind" = "INTERNAL",
                    "authentication.jwt.failed" = tracing::field::Empty,
                    "authentication.jwt.source" = tracing::field::Empty
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
            if let Source::Header { value_prefix, .. } = source
                && value_prefix.as_bytes().iter().any(u8::is_ascii_whitespace)
            {
                return Err(Error::BadHeaderValuePrefix.into());
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
                issuers: jwks_conf.issuers.clone(),
                audiences: jwks_conf.audiences.clone(),
                algorithms: jwks_conf
                    .algorithms
                    .as_ref()
                    .map(|algs| algs.iter().cloned().collect()),
                poll_interval: jwks_conf.poll_interval,
                headers: jwks_conf.headers.clone(),
            });
        }

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
    fn new_failure(source: Option<&Source>, error_context: ErrorContext) -> Self {
        let (r#type, name) = match source {
            Some(Source::Header { name, .. }) => ("header", name.as_str()),
            Some(Source::Cookie { name }) => ("cookie", name.as_str()),
            None => ("unknown", "unknown"),
        };

        Self::Failure {
            r#type: r#type.into(),
            name: name.into(),
            error: error_context,
        }
    }

    fn new_success(source: Option<&Source>) -> Self {
        match source {
            Some(Source::Header { name, .. }) => Self::Success {
                r#type: "header".into(),
                name: name.into(),
            },
            Some(Source::Cookie { name }) => Self::Success {
                r#type: "cookie".into(),
                name: name.into(),
            },
            None => Self::Success {
                r#type: "unknown".into(),
                name: "unknown".into(),
            },
        }
    }

    #[cfg(test)]
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
        source: Option<&Source>,
    ) -> ControlFlow<router::Response, router::Request> {
        // This is a metric and will not appear in the logs
        let failed = true;
        increment_jwt_counter_metric(failed, source);

        // Record span attributes for JWT failure
        let span = tracing::Span::current();
        span.record("authentication.jwt.failed", true);
        if let Some(src) = source {
            span.record("authentication.jwt.source", src.as_textual_representation());
            tracing::info!(message = %error, jwtsource = %src.as_textual_representation(), "jwt authentication failure");
        } else {
            tracing::info!(message = %error, "jwt authentication failure");
        }

        let _ = request.context.insert_json_value(
            JWT_CONTEXT_KEY,
            serde_json_bytes::json!(JwtStatus::new_failure(source, error.as_context_object())),
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

    /// This is the documented metric
    ///
    /// Emits a counter with the following attributes:
    /// - `authentication.jwt.failed`: boolean indicating if authentication failed
    /// - `authentication.jwt.source`: source of the JWT (e.g., "header:authorization", "cookie:authz") when available
    fn increment_jwt_counter_metric(failed: bool, source: Option<&Source>) {
        if let Some(src) = source {
            u64_counter!(
                "apollo.router.operations.authentication.jwt",
                "Number of requests with JWT authentication",
                1,
                authentication.jwt.failed = failed,
                authentication.jwt.source = src.as_textual_representation()
            );
        } else {
            u64_counter!(
                "apollo.router.operations.authentication.jwt",
                "Number of requests with JWT authentication",
                1,
                authentication.jwt.failed = failed
            );
        }
    }

    let mut jwt = None;
    let mut source_of_extracted_jwt = None;
    for source in &config.sources {
        let extracted_jwt = jwks::extract_jwt(
            source,
            config.ignore_other_prefixes,
            request.router_request.headers(),
        );

        match extracted_jwt {
            None => continue,
            Some(Ok(extracted_jwt)) => {
                source_of_extracted_jwt = Some(source);
                jwt = Some(extracted_jwt);
                break;
            }
            Some(Err(error)) => {
                return failure_message(
                    request,
                    config,
                    error,
                    StatusCode::BAD_REQUEST,
                    Some(source),
                );
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
                source_of_extracted_jwt,
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
        let (issuers, audiences, token_data) = match jwks::decode_jwt(jwt, keys, criteria) {
            Ok(data) => data,
            Err((auth_error, status_code)) => {
                return failure_message(
                    request,
                    config,
                    auth_error,
                    status_code,
                    source_of_extracted_jwt,
                );
            }
        };

        if let Some(configured_issuers) = issuers
            && let Some(token_issuer) = token_data
                .claims
                .as_object()
                .and_then(|o| o.get("iss"))
                .and_then(|value| value.as_str())
            && !configured_issuers.contains(token_issuer)
        {
            let mut issuers_for_error: Vec<String> = configured_issuers.into_iter().collect();
            issuers_for_error.sort(); // done to maintain consistent ordering in error message
            return failure_message(
                request,
                config,
                AuthenticationError::InvalidIssuer {
                    expected: issuers_for_error
                        .iter()
                        .map(|issuer| issuer.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    token: token_issuer.to_string(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
                source_of_extracted_jwt,
            );
        }

        if let Some(configured_audiences) = audiences {
            let maybe_token_audiences = token_data.claims.as_object().and_then(|o| o.get("aud"));
            let Some(maybe_token_audiences) = maybe_token_audiences else {
                let mut audiences_for_error: Vec<String> =
                    configured_audiences.into_iter().collect();
                audiences_for_error.sort(); // done to maintain consistent ordering in error message
                return failure_message(
                    request,
                    config,
                    AuthenticationError::InvalidAudience {
                        expected: audiences_for_error
                            .iter()
                            .map(|audience| audience.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        actual: "<none>".to_string(),
                    },
                    StatusCode::UNAUTHORIZED,
                    source_of_extracted_jwt,
                );
            };

            if let Some(token_audience) = maybe_token_audiences.as_str() {
                if !configured_audiences.contains(token_audience) {
                    let mut audiences_for_error: Vec<String> =
                        configured_audiences.into_iter().collect();
                    audiences_for_error.sort(); // done to maintain consistent ordering in error message
                    return failure_message(
                        request,
                        config,
                        AuthenticationError::InvalidAudience {
                            expected: audiences_for_error
                                .iter()
                                .map(|audience| audience.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            actual: token_audience.to_string(),
                        },
                        StatusCode::UNAUTHORIZED,
                        source_of_extracted_jwt,
                    );
                }
            } else {
                // If the token has incorrectly configured audiences, we cannot validate it against
                // the configured audiences.
                let mut audiences_for_error: Vec<String> =
                    configured_audiences.into_iter().collect();
                audiences_for_error.sort(); // done to maintain consistent ordering in error message
                return failure_message(
                    request,
                    config,
                    AuthenticationError::InvalidAudience {
                        expected: audiences_for_error
                            .iter()
                            .map(|audience| audience.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        actual: maybe_token_audiences.to_string(),
                    },
                    StatusCode::UNAUTHORIZED,
                    source_of_extracted_jwt,
                );
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
                source_of_extracted_jwt,
            );
        }
        // This is a metric and will not appear in the logs
        //
        // Apparently intended to be `apollo.router.operations.authentication.jwt` like above,
        // but has existed for two years with a buggy name. Keep it for now.
        u64_counter!(
            "apollo.router.operations.jwt",
            "Number of requests with JWT successful authentication (deprecated, \
                use `apollo.router.operations.authentication.jwt` \
                with `authentication.jwt.failed = false` instead)",
            1
        );
        // Use the fixed name too:
        let failed = false;
        increment_jwt_counter_metric(failed, source_of_extracted_jwt);

        // Record span attributes for JWT success
        let span = tracing::Span::current();
        span.record("authentication.jwt.failed", false);
        if let Some(src) = source_of_extracted_jwt {
            span.record("authentication.jwt.source", src.as_textual_representation());
        }

        let _ = request.context.insert_json_value(
            JWT_CONTEXT_KEY,
            serde_json_bytes::json!(JwtStatus::new_success(source_of_extracted_jwt)),
        );

        return ControlFlow::Continue(request);
    }

    // We can't find a key to process this JWT.
    let err = criteria.kid.map_or_else(
        || AuthenticationError::CannotFindSuitableKey(criteria.alg, None),
        AuthenticationError::CannotFindKID,
    );

    failure_message(
        request,
        config,
        err,
        StatusCode::UNAUTHORIZED,
        source_of_extracted_jwt,
    )
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_private_plugin!("apollo", "authentication", AuthenticationPlugin);
