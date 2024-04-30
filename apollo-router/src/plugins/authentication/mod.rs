//! Authentication plugin

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use displaydoc::Display;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use jsonwebtoken::decode;
use jsonwebtoken::decode_header;
use jsonwebtoken::errors::Error as JWTError;
use jsonwebtoken::jwk::AlgorithmParameters;
use jsonwebtoken::jwk::EllipticCurve;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::jwk::KeyAlgorithm;
use jsonwebtoken::jwk::KeyOperations;
use jsonwebtoken::jwk::PublicKeyUse;
use jsonwebtoken::Algorithm;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::TokenData;
use jsonwebtoken::Validation;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
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
use crate::plugin::serde::deserialize_header_name;
use crate::plugin::serde::deserialize_header_value;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authentication::jwks::JwkSetInfo;
use crate::plugins::authentication::jwks::JwksConfig;
use crate::register_plugin;
use crate::services::router;
use crate::Context;

mod jwks;
pub(crate) mod subgraph;

#[cfg(test)]
mod tests;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication_plugin";
pub(crate) const APOLLO_AUTHENTICATION_JWT_CLAIMS: &str = "apollo_authentication::JWT::claims";
const HEADER_TOKEN_TRUNCATED: &str = "(truncated)";

#[derive(Debug, Display, Error)]
pub(crate) enum AuthenticationError<'a> {
    /// Configured header is not convertible to a string
    CannotConvertToString,

    /// Header Value: '{0}' is not correctly formatted. prefix should be '{1}'
    InvalidPrefix(&'a str, &'a str),

    /// Header Value: '{0}' is not correctly formatted. Missing JWT
    MissingJWT(&'a str),

    /// '{0}' is not a valid JWT header: {1}
    InvalidHeader(&'a str, JWTError),

    /// Cannot create decoding key: {0}
    CannotCreateDecodingKey(JWTError),

    /// JWK does not contain an algorithm
    JWKHasNoAlgorithm,

    /// Cannot decode JWT: {0}
    CannotDecodeJWT(JWTError),

    /// Cannot insert claims into context: {0}
    CannotInsertClaimsIntoContext(BoxError),

    /// Cannot find kid: '{0:?}' in JWKS list
    CannotFindKID(Option<String>),

    /// Cannot find a suitable key for: alg: '{0:?}', kid: '{1:?}' in JWKS list
    CannotFindSuitableKey(Algorithm, Option<String>),

    /// Invalid issuer: the token's `iss` was '{token}', but signed with a key from '{expected}'
    InvalidIssuer { expected: String, token: String },

    /// Unsupported key algorithm: {0}
    UnsupportedKeyAlgorithm(KeyAlgorithm),
}

const DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL: Duration = Duration::from_secs(60);

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| Ok(Client::new()));

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("header_value_prefix must not contain whitespace")]
    BadHeaderValuePrefix,
}

struct Router {
    configuration: JWTConf,
    jwks_manager: JwksManager,
}

struct AuthenticationPlugin {
    router: Option<Router>,
    subgraph: Option<SubgraphAuth>,
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
    http::header::AUTHORIZATION.to_string()
}

fn default_header_value_prefix() -> String {
    "Bearer".to_string()
}

fn default_poll_interval() -> Duration {
    DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL
}

#[derive(Debug, Default)]
struct JWTCriteria {
    alg: Algorithm,
    kid: Option<String>,
}

/// Search the list of JWKS to find a key we can use to decode a JWT.
///
/// The search criteria allow us to match a variety of keys depending on which criteria are provided
/// by the JWT header. The only mandatory parameter is "alg".
/// Note: "none" is not implemented by jsonwebtoken, so it can't be part of the [`Algorithm`] enum.
fn search_jwks(
    jwks_manager: &JwksManager,
    criteria: &JWTCriteria,
) -> Option<Vec<(Option<String>, Jwk)>> {
    const HIGHEST_SCORE: usize = 2;
    let mut candidates = vec![];
    let mut found_highest_score = false;
    for JwkSetInfo {
        jwks,
        issuer,
        algorithms,
    } in jwks_manager.iter_jwks()
    {
        // filter accepted algorithms
        if let Some(algs) = algorithms {
            if !algs.contains(&criteria.alg) {
                continue;
            }
        }

        // Try to figure out if our jwks contains a candidate key (i.e.: a key which matches our
        // criteria)
        for mut key in jwks.keys.into_iter().filter(|key| {
            // We are only interested in keys which are used for signature verification
            match (&key.common.public_key_use, &key.common.key_operations) {
                // "use" https://datatracker.ietf.org/doc/html/rfc7517#section-4.2 and
                // "key_ops" https://datatracker.ietf.org/doc/html/rfc7517#section-4.3 are both optional
                (None, None) => true,
                (None, Some(purpose)) => purpose.contains(&KeyOperations::Verify),
                (Some(key_use), None) => key_use == &PublicKeyUse::Signature,
                // The "use" and "key_ops" JWK members SHOULD NOT be used together;
                // however, if both are used, the information they convey MUST be
                // consistent
                (Some(key_use), Some(purpose)) => {
                    key_use == &PublicKeyUse::Signature && purpose.contains(&KeyOperations::Verify)
                }
            }
        }) {
            let mut key_score = 0;

            // Let's see if we have a specified kid and if they match
            if criteria.kid.is_some() && key.common.key_id == criteria.kid {
                key_score += 1;
            }

            // Furthermore, we would like our algorithms to match, or at least the kty
            // If we have an algorithm that matches, boost the score
            match key.common.key_algorithm {
                Some(algorithm) => {
                    if convert_key_algorithm(algorithm) != Some(criteria.alg) {
                        continue;
                    }
                    key_score += 1;
                }
                // If a key doesn't have an algorithm, then we match the "alg" specified in the
                // search criteria against all of the algorithms that we support.  If the
                // key.algorithm parameters match the type of parameters for the "family" of the
                // criteria "alg", then we update the key to use the value of "alg" provided in
                // the search criteria.
                // If not, then this is not a usable key for this JWT
                // Note: Matching algorithm parameters may seem unusual, but the appropriate
                // algorithm details are not structured for easy consumption in jsonwebtoken and
                // this is the simplest way to determine algorithm family.
                None => match (criteria.alg, &key.algorithm) {
                    (
                        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512,
                        AlgorithmParameters::OctetKey(_),
                    ) => {
                        key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                    }
                    (
                        Algorithm::RS256
                        | Algorithm::RS384
                        | Algorithm::RS512
                        | Algorithm::PS256
                        | Algorithm::PS384
                        | Algorithm::PS512,
                        AlgorithmParameters::RSA(_),
                    ) => {
                        key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                    }
                    (Algorithm::ES256, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::P256 {
                            key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                        }
                    }
                    (Algorithm::ES384, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::P384 {
                            key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                        }
                    }
                    (Algorithm::EdDSA, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::Ed25519 {
                            key.common.key_algorithm = Some(convert_algorithm(criteria.alg));
                        }
                    }
                    _ => {
                        // We'll ignore combinations we don't recognise
                        continue;
                    }
                },
            };

            // If we get here we have a key that:
            //  - may be used for signature verification
            //  - has a matching algorithm, or if JWT has no algorithm, a matching key type
            // It may have a matching kid if the JWT has a kid and it matches the key kid
            //
            // Multiple keys may meet the matching criteria, but they have a score. They get 1
            // point for having an explicitly matching algorithm and 1 point for an explicitly
            // matching kid. We will sort our candidates and pick the key with the highest score.

            // If we find a key with a HIGHEST_SCORE, we will filter the list to only keep those
            // with that score
            if key_score == HIGHEST_SCORE {
                found_highest_score = true;
            }

            candidates.push((key_score, (issuer.clone(), key)));
        }
    }

    tracing::debug!(
        "jwk candidates: {:?}",
        candidates
            .iter()
            .map(|(score, (_, candidate))| (
                score,
                &candidate.common.key_id,
                candidate.common.key_algorithm
            ))
            .collect::<Vec<(&usize, &Option<String>, Option<KeyAlgorithm>)>>()
    );

    if candidates.is_empty() {
        None
    } else {
        // Only sort if we need to
        if candidates.len() > 1 {
            candidates.sort_by(|a, b| a.0.cmp(&b.0));
        }

        if found_highest_score {
            Some(
                candidates
                    .into_iter()
                    .filter_map(|(score, candidate)| {
                        if score == HIGHEST_SCORE {
                            Some(candidate)
                        } else {
                            None
                        }
                    })
                    .collect(),
            )
        } else {
            Some(
                candidates
                    .into_iter()
                    .map(|(_score, candidate)| candidate)
                    .collect(),
            )
        }
    }
}

#[async_trait::async_trait]
impl Plugin for AuthenticationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let subgraph = if let Some(config) = init.config.subgraph {
            let all = if let Some(config) = &config.all {
                Some(subgraph::make_signing_params(config, "all").await?)
            } else {
                None
            };

            let mut subgraphs: HashMap<String, SigningParamsConfig> = Default::default();
            for (subgraph_name, config) in &config.subgraphs {
                subgraphs.insert(
                    subgraph_name.clone(),
                    subgraph::make_signing_params(config, subgraph_name.as_str()).await?,
                );
            }

            Some(SubgraphAuth {
                signing_params: { SigningParams { all, subgraphs } },
            })
        } else {
            None
        };

        let router = if let Some(mut router_conf) = init.config.router {
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

            Some(Router {
                configuration: router_conf.jwt,
                jwks_manager,
            })
        } else {
            None
        };

        Ok(Self { router, subgraph })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        if let Some(config) = &self.router {
            let jwks_manager = config.jwks_manager.clone();
            let configuration = config.configuration.clone();

            fn authentication_service_span() -> impl Fn(&router::Request) -> tracing::Span + Clone {
                move |_request: &router::Request| {
                    tracing::info_span!(
                        AUTHENTICATION_SPAN_NAME,
                        "authentication service" = stringify!(router::Request),
                        "otel.kind" = "INTERNAL"
                    )
                }
            }

            ServiceBuilder::new()
                .instrument(authentication_service_span())
                .checkpoint(move |request: router::Request| {
                    Ok(authenticate(&configuration, &jwks_manager, request))
                })
                .service(service)
                .boxed()
        } else {
            service
        }
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: crate::services::subgraph::BoxService,
    ) -> crate::services::subgraph::BoxService {
        if let Some(auth) = &self.subgraph {
            auth.subgraph_service(name, service)
        } else {
            service
        }
    }
}

fn authenticate(
    config: &JWTConf,
    jwks_manager: &JwksManager,
    request: router::Request,
) -> ControlFlow<router::Response, router::Request> {
    const AUTHENTICATION_KIND: &str = "JWT";

    // We are going to do a lot of similar checking so let's define a local function
    // to help reduce repetition
    fn failure_message(
        context: Context,
        error: AuthenticationError,
        status: StatusCode,
    ) -> ControlFlow<router::Response, router::Request> {
        // This is a metric and will not appear in the logs
        tracing::info!(
            monotonic_counter.apollo_authentication_failure_count = 1u64,
            kind = %AUTHENTICATION_KIND
        );
        tracing::info!(
            monotonic_counter
                .apollo
                .router
                .operations
                .authentication
                .jwt = 1,
            authentication.jwt.failed = true
        );
        tracing::info!(message = %error, "jwt authentication failure");
        let response = router::Response::infallible_builder()
            .error(
                graphql::Error::builder()
                    .message(error.to_string())
                    .extension_code("AUTH_ERROR")
                    .build(),
            )
            .status_code(status)
            .context(context)
            .build();
        ControlFlow::Break(response)
    }

    let mut jwt = None;
    for source in &config.sources {
        match extract_jwt(
            source,
            config.ignore_other_prefixes,
            request.router_request.headers(),
        ) {
            None => continue,
            Some(Err(error)) => {
                return failure_message(request.context, error, StatusCode::BAD_REQUEST)
            }
            Some(Ok(extracted_jwt)) => {
                jwt = Some(extracted_jwt);
                break;
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
                request.context,
                AuthenticationError::InvalidHeader(HEADER_TOKEN_TRUNCATED, e),
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // Extract our search criteria from our jwt
    let criteria = JWTCriteria {
        kid: jwt_header.kid,
        alg: jwt_header.alg,
    };

    // Search our list of JWKS to find the kid and process it
    // Note: This will search through JWKS in the order in which they are defined
    // in configuration.
    if let Some(keys) = search_jwks(jwks_manager, &criteria) {
        let (issuer, token_data) = match decode_jwt(jwt, keys, criteria) {
            Ok(data) => data,
            Err((auth_error, status_code)) => {
                return failure_message(request.context, auth_error, status_code);
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
                        request.context,
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
            .insert(APOLLO_AUTHENTICATION_JWT_CLAIMS, token_data.claims)
        {
            return failure_message(
                request.context,
                AuthenticationError::CannotInsertClaimsIntoContext(e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
        // This is a metric and will not appear in the logs
        tracing::info!(
            monotonic_counter.apollo_authentication_success_count = 1u64,
            kind = %AUTHENTICATION_KIND
        );
        tracing::info!(monotonic_counter.apollo.router.operations.jwt = 1u64);
        return ControlFlow::Continue(request);
    }

    // We can't find a key to process this JWT.
    if criteria.kid.is_some() {
        failure_message(
            request.context,
            AuthenticationError::CannotFindKID(criteria.kid),
            StatusCode::UNAUTHORIZED,
        )
    } else {
        failure_message(
            request.context,
            AuthenticationError::CannotFindSuitableKey(criteria.alg, criteria.kid),
            StatusCode::UNAUTHORIZED,
        )
    }
}

fn extract_jwt<'a, 'b: 'a>(
    source: &'a Source,
    ignore_other_prefixes: bool,
    headers: &'b HeaderMap,
) -> Option<Result<&'b str, AuthenticationError<'a>>> {
    match source {
        Source::Header { name, value_prefix } => {
            // The http_request is stored in a `Router::Request` context.
            // We are going to check the headers for the presence of the configured header
            let jwt_value_result = match headers.get(name) {
                Some(value) => value.to_str(),
                None => return None,
            };

            // If we find the header, but can't convert it to a string, let the client know
            let jwt_value_untrimmed = match jwt_value_result {
                Ok(value) => value,
                Err(_not_a_string_error) => {
                    return Some(Err(AuthenticationError::CannotConvertToString));
                }
            };

            // Let's trim out leading and trailing whitespace to be accommodating
            let jwt_value = jwt_value_untrimmed.trim();

            // Make sure the format of our message matches our expectations
            // Technically, the spec is case sensitive, but let's accept
            // case variations
            //
            let prefix_len = value_prefix.len();
            if jwt_value.len() < prefix_len
                || !&jwt_value[..prefix_len].eq_ignore_ascii_case(value_prefix)
            {
                if ignore_other_prefixes {
                    return None;
                } else {
                    return Some(Err(AuthenticationError::InvalidPrefix(
                        jwt_value_untrimmed,
                        value_prefix,
                    )));
                }
            }
            // If there's no header prefix, we need to avoid splitting the header
            let jwt = if value_prefix.is_empty() {
                // check for whitespace- we've already trimmed, so this means the request has a prefix that shouldn't exist
                if jwt_value.contains(' ') {
                    return Some(Err(AuthenticationError::InvalidPrefix(
                        jwt_value_untrimmed,
                        value_prefix,
                    )));
                }

                // we can simply assign the jwt to the jwt_value; we'll validate down below
                jwt_value
            } else {
                // Otherwise, we need to split our string in (at most 2) sections.
                let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                if jwt_parts.len() != 2 {
                    return Some(Err(AuthenticationError::MissingJWT(jwt_value)));
                }

                // We have our jwt
                jwt_parts[1]
            };
            Some(Ok(jwt))
        }
        Source::Cookie { name } => {
            for header in headers.get_all("cookie") {
                let value = match header.to_str() {
                    Ok(value) => value,
                    Err(_not_a_string_error) => {
                        return Some(Err(AuthenticationError::CannotConvertToString));
                    }
                };
                for cookie in cookie::Cookie::split_parse(value) {
                    match cookie {
                        Err(_) => continue,
                        Ok(cookie) => {
                            if cookie.name() == name {
                                if let Some(value) = cookie.value_raw() {
                                    return Some(Ok(value));
                                }
                            }
                        }
                    }
                }
            }

            None
        }
    }
}

fn decode_jwt(
    jwt: &str,
    keys: Vec<(Option<String>, Jwk)>,
    criteria: JWTCriteria,
) -> Result<(Option<String>, TokenData<serde_json::Value>), (AuthenticationError, StatusCode)> {
    let mut error = None;
    for (issuer, jwk) in keys.into_iter() {
        let decoding_key = match DecodingKey::from_jwk(&jwk) {
            Ok(k) => k,
            Err(e) => {
                error = Some((
                    AuthenticationError::CannotCreateDecodingKey(e),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
                continue;
            }
        };

        let key_algorithm = match jwk.common.key_algorithm {
            Some(a) => a,
            None => {
                error = Some((
                    AuthenticationError::JWKHasNoAlgorithm,
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
                continue;
            }
        };

        let algorithm = match convert_key_algorithm(key_algorithm) {
            Some(a) => a,
            None => {
                error = Some((
                    AuthenticationError::UnsupportedKeyAlgorithm(key_algorithm),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
                continue;
            }
        };

        let mut validation = Validation::new(algorithm);
        validation.validate_nbf = true;
        // if set to true, it will reject tokens containing an `aud` claim if the validation does not specify an audience
        // we don't validate audience yet, so this is deactivated
        validation.validate_aud = false;

        match decode::<serde_json::Value>(jwt, &decoding_key, &validation) {
            Ok(v) => return Ok((issuer, v)),
            Err(e) => {
                error = Some((
                    AuthenticationError::CannotDecodeJWT(e),
                    StatusCode::UNAUTHORIZED,
                ));
            }
        };
    }

    match error {
        Some(e) => Err(e),
        None => {
            // We can't find a key to process this JWT.
            if criteria.kid.is_some() {
                Err((
                    AuthenticationError::CannotFindKID(criteria.kid),
                    StatusCode::UNAUTHORIZED,
                ))
            } else {
                Err((
                    AuthenticationError::CannotFindSuitableKey(criteria.alg, criteria.kid),
                    StatusCode::UNAUTHORIZED,
                ))
            }
        }
    }
}

pub(crate) fn jwt_expires_in(context: &Context) -> Duration {
    let claims = context
        .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .map_err(|err| tracing::error!("could not read JWT claims: {err}"))
        .ok()
        .flatten();
    let ts_opt = claims.as_ref().and_then(|x: &Value| {
        if !x.is_object() {
            tracing::error!("JWT claims should be an object");
            return None;
        }
        let claims = x.as_object().expect("claims should be an object");
        let exp = claims.get("exp")?;
        if !exp.is_number() {
            tracing::error!("JWT 'exp' (expiry) claim should be a number");
            return None;
        }
        exp.as_i64()
    });
    match ts_opt {
        Some(ts) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("we should not run before EPOCH")
                .as_secs() as i64;
            if now < ts {
                Duration::from_secs((ts - now) as u64)
            } else {
                Duration::ZERO
            }
        }
        None => Duration::MAX,
    }
}

//Apparently the jsonwebtoken crate now has 2 different enums for algorithms
pub(crate) fn convert_key_algorithm(algorithm: KeyAlgorithm) -> Option<Algorithm> {
    Some(match algorithm {
        jsonwebtoken::jwk::KeyAlgorithm::HS256 => jsonwebtoken::Algorithm::HS256,
        jsonwebtoken::jwk::KeyAlgorithm::HS384 => jsonwebtoken::Algorithm::HS384,
        jsonwebtoken::jwk::KeyAlgorithm::HS512 => jsonwebtoken::Algorithm::HS512,
        jsonwebtoken::jwk::KeyAlgorithm::ES256 => jsonwebtoken::Algorithm::ES256,
        jsonwebtoken::jwk::KeyAlgorithm::ES384 => jsonwebtoken::Algorithm::ES384,
        jsonwebtoken::jwk::KeyAlgorithm::RS256 => jsonwebtoken::Algorithm::RS256,
        jsonwebtoken::jwk::KeyAlgorithm::RS384 => jsonwebtoken::Algorithm::RS384,
        jsonwebtoken::jwk::KeyAlgorithm::RS512 => jsonwebtoken::Algorithm::RS512,
        jsonwebtoken::jwk::KeyAlgorithm::PS256 => jsonwebtoken::Algorithm::PS256,
        jsonwebtoken::jwk::KeyAlgorithm::PS384 => jsonwebtoken::Algorithm::PS384,
        jsonwebtoken::jwk::KeyAlgorithm::PS512 => jsonwebtoken::Algorithm::PS512,
        jsonwebtoken::jwk::KeyAlgorithm::EdDSA => jsonwebtoken::Algorithm::EdDSA,
        // we do not use the encryption algorithms
        jsonwebtoken::jwk::KeyAlgorithm::RSA1_5
        | jsonwebtoken::jwk::KeyAlgorithm::RSA_OAEP
        | jsonwebtoken::jwk::KeyAlgorithm::RSA_OAEP_256 => return None,
    })
}

pub(crate) fn convert_algorithm(algorithm: Algorithm) -> KeyAlgorithm {
    match algorithm {
        jsonwebtoken::Algorithm::HS256 => jsonwebtoken::jwk::KeyAlgorithm::HS256,
        jsonwebtoken::Algorithm::HS384 => jsonwebtoken::jwk::KeyAlgorithm::HS384,
        jsonwebtoken::Algorithm::HS512 => jsonwebtoken::jwk::KeyAlgorithm::HS512,
        jsonwebtoken::Algorithm::ES256 => jsonwebtoken::jwk::KeyAlgorithm::ES256,
        jsonwebtoken::Algorithm::ES384 => jsonwebtoken::jwk::KeyAlgorithm::ES384,
        jsonwebtoken::Algorithm::RS256 => jsonwebtoken::jwk::KeyAlgorithm::RS256,
        jsonwebtoken::Algorithm::RS384 => jsonwebtoken::jwk::KeyAlgorithm::RS384,
        jsonwebtoken::Algorithm::RS512 => jsonwebtoken::jwk::KeyAlgorithm::RS512,
        jsonwebtoken::Algorithm::PS256 => jsonwebtoken::jwk::KeyAlgorithm::PS256,
        jsonwebtoken::Algorithm::PS384 => jsonwebtoken::jwk::KeyAlgorithm::PS384,
        jsonwebtoken::Algorithm::PS512 => jsonwebtoken::jwk::KeyAlgorithm::PS512,
        jsonwebtoken::Algorithm::EdDSA => jsonwebtoken::jwk::KeyAlgorithm::EdDSA,
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("apollo", "authentication", AuthenticationPlugin);
