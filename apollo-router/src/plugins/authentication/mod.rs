//! Authentication plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::ops::ControlFlow;
use std::str::FromStr;
use std::time::Duration;

use displaydoc::Display;
use http::StatusCode;
use jsonwebtoken::decode;
use jsonwebtoken::decode_header;
use jsonwebtoken::errors::Error as JWTError;
use jsonwebtoken::jwk::AlgorithmParameters;
use jsonwebtoken::jwk::EllipticCurve;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::jwk::KeyOperations;
use jsonwebtoken::jwk::PublicKeyUse;
use jsonwebtoken::Algorithm;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::Validation;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

use self::jwks::JwksManager;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::authentication::jwks::JwkSetInfo;
use crate::plugins::authentication::jwks::JwksConfig;
use crate::register_plugin;
use crate::services::router;
use crate::Context;

mod jwks;
#[cfg(test)]
mod tests;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication_plugin";
pub(crate) const APOLLO_AUTHENTICATION_JWT_CLAIMS: &str = "apollo_authentication::JWT::claims";
const HEADER_TOKEN_TRUNCATED: &str = "(truncated)";

#[derive(Debug, Display, Error)]
enum AuthenticationError<'a> {
    /// Configured header is not convertible to a string
    CannotConvertToString,

    /// Header Value: '{0}' is not correctly formatted. prefix should be '{1}'
    InvalidPrefix(&'a str, &'a str),

    /// Header Value: '{0}' is not correctly formatted. Missing JWT
    MissingJWT(&'a str),

    /// '{0}' is not a valid JWT header: {1}
    InvalidHeader(&'a str, JWTError),

    /// Cannot retrieve JWKS: {0}
    CannotRetrieveJWKS(BoxError),

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
}

const DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL: Duration = Duration::from_secs(60);

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| Ok(Client::new()));

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("header_value_prefix must not contain whitespace")]
    BadHeaderValuePrefix,
}

struct AuthenticationPlugin {
    configuration: JWTConf,
    jwks_manager: JwksManager,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct JWTConf {
    /// List of JWKS used to verify tokens
    jwks: Vec<JwksConf>,
    /// HTTP header expected to contain JWT
    #[serde(default = "default_header_name")]
    header_name: String,
    /// Header value prefix
    #[serde(default = "default_header_value_prefix")]
    header_value_prefix: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct JwksConf {
    /// Retrieve the JWK Set
    url: String,
    /// Expected issuer for tokens verified by that JWKS
    issuer: Option<String>,
    /// List of accepted algorithms. Possible values are `HS256`, `HS384`, `HS512`, `ES256`, `ES384`, `RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`, `EdDSA`
    #[schemars(with = "Option<Vec<String>>", default)]
    #[serde(default)]
    algorithms: Option<Vec<Algorithm>>,
}

impl Default for JWTConf {
    fn default() -> Self {
        Self {
            jwks: Default::default(),
            header_name: default_header_name(),
            header_value_prefix: default_header_value_prefix(),
        }
    }
}

// We may support additional authentication mechanisms in future, so all
// configuration (which is currently JWT specific) is isolated to the
// JWTConf structure.
/// Authentication
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    /// The JWT configuration
    jwt: JWTConf,
}

fn default_header_name() -> String {
    http::header::AUTHORIZATION.to_string()
}

fn default_header_value_prefix() -> String {
    "Bearer".to_string()
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
) -> Result<Option<(Option<String>, Jwk)>, BoxError> {
    const HIGHEST_SCORE: usize = 2;
    let mut candidates = vec![];
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
            if let Some(purpose) = &key.common.public_key_use {
                purpose == &PublicKeyUse::Signature
            } else if let Some(purpose) = &key.common.key_operations {
                purpose.contains(&KeyOperations::Verify)
            } else {
                false
            }
        }) {
            let mut key_score = 0;

            // Let's see if we have a specified kid and if they match
            if criteria.kid.is_some() && key.common.key_id == criteria.kid {
                key_score += 1;
            }

            // Furthermore, we would like our algorithms to match, or at least the kty
            // If we have an algorithm that matches, boost the score
            match key.common.algorithm {
                Some(algorithm) => {
                    if algorithm != criteria.alg {
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
                        key.common.algorithm = Some(criteria.alg);
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
                        key.common.algorithm = Some(criteria.alg);
                    }
                    (Algorithm::ES256, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::P256 {
                            key.common.algorithm = Some(criteria.alg);
                        }
                    }
                    (Algorithm::ES384, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::P384 {
                            key.common.algorithm = Some(criteria.alg);
                        }
                    }
                    (Algorithm::EdDSA, AlgorithmParameters::EllipticCurve(params)) => {
                        if params.curve == EllipticCurve::Ed25519 {
                            key.common.algorithm = Some(criteria.alg);
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

            // If we find a key with a HIGHEST_SCORE, let's stop looking.
            if key_score == HIGHEST_SCORE {
                return Ok(Some((issuer, key)));
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
                candidate.common.algorithm
            ))
            .collect::<Vec<(&usize, &Option<String>, Option<Algorithm>)>>()
    );

    if candidates.is_empty() {
        Ok(None)
    } else {
        // Only sort if we need to
        if candidates.len() > 1 {
            candidates.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Ok(Some(candidates.pop().expect("list isn't empty").1))
    }
}

#[async_trait::async_trait]
impl Plugin for AuthenticationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        if init
            .config
            .jwt
            .header_value_prefix
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            return Err(Error::BadHeaderValuePrefix.into());
        }
        let mut list = vec![];
        for jwks_conf in &init.config.jwt.jwks {
            let url: Url = Url::from_str(jwks_conf.url.as_str())?;
            list.push(JwksConfig {
                url,
                issuer: jwks_conf.issuer.clone(),
                algorithms: jwks_conf
                    .algorithms
                    .as_ref()
                    .map(|algs| algs.iter().cloned().collect()),
            });
        }

        tracing::info!(jwks=?init.config.jwt.jwks, "JWT authentication using JWKSets from");

        let jwks_manager = JwksManager::new(list).await?;

        Ok(AuthenticationPlugin {
            configuration: init.config.jwt,
            jwks_manager,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let request_full_config = self.configuration.clone();
        let jwks_manager = self.jwks_manager.clone();

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
                authenticate(&request_full_config, &jwks_manager, request)
            })
            .service(service)
            .boxed()
    }
}

fn authenticate(
    config: &JWTConf,
    jwks_manager: &JwksManager,
    request: router::Request,
) -> Result<ControlFlow<router::Response, router::Request>, BoxError> {
    const AUTHENTICATION_KIND: &str = "JWT";

    // We are going to do a lot of similar checking so let's define a local function
    // to help reduce repetition
    fn failure_message(
        context: Context,
        error: AuthenticationError,
        status: StatusCode,
    ) -> Result<ControlFlow<router::Response, router::Request>, BoxError> {
        // This is a metric and will not appear in the logs
        tracing::info!(
            monotonic_counter.apollo_authentication_failure_count = 1u64,
            kind = %AUTHENTICATION_KIND
        );
        tracing::info!(message = %error, "jwt authentication failure");
        let response = router::Response::error_builder()
            .error(
                graphql::Error::builder()
                    .message(error.to_string())
                    .extension_code("AUTH_ERROR")
                    .build(),
            )
            .status_code(status)
            .context(context)
            .build()?;
        Ok(ControlFlow::Break(response))
    }

    // The http_request is stored in a `Router::Request` context.
    // We are going to check the headers for the presence of the configured header
    let jwt_value_result = match request.router_request.headers().get(&config.header_name) {
        Some(value) => value.to_str(),
        None => {
            return Ok(ControlFlow::Continue(request));
        }
    };

    // If we find the header, but can't convert it to a string, let the client know
    let jwt_value_untrimmed = match jwt_value_result {
        Ok(value) => value,
        Err(_not_a_string_error) => {
            return failure_message(
                request.context,
                AuthenticationError::CannotConvertToString,
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // Let's trim out leading and trailing whitespace to be accommodating
    let jwt_value = jwt_value_untrimmed.trim();

    // Make sure the format of our message matches our expectations
    // Technically, the spec is case sensitive, but let's accept
    // case variations
    //
    let prefix_len = config.header_value_prefix.len();
    if jwt_value.len() < prefix_len
        || !&jwt_value[..prefix_len].eq_ignore_ascii_case(&config.header_value_prefix)
    {
        return failure_message(
            request.context,
            AuthenticationError::InvalidPrefix(jwt_value_untrimmed, &config.header_value_prefix),
            StatusCode::BAD_REQUEST,
        );
    }

    // Split our string in (at most 2) sections.
    let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
    if jwt_parts.len() != 2 {
        return failure_message(
            request.context,
            AuthenticationError::MissingJWT(jwt_value),
            StatusCode::BAD_REQUEST,
        );
    }

    // We have our jwt
    let jwt = jwt_parts[1];

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

    let jwk_opt = match search_jwks(jwks_manager, &criteria) {
        Ok(j) => j,
        Err(e) => {
            return failure_message(
                request.context,
                AuthenticationError::CannotRetrieveJWKS(e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if let Some((issuer, jwk)) = jwk_opt {
        let decoding_key = match DecodingKey::from_jwk(&jwk) {
            Ok(k) => k,
            Err(e) => {
                return failure_message(
                    request.context,
                    AuthenticationError::CannotCreateDecodingKey(e),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        let algorithm = match jwk.common.algorithm {
            Some(a) => a,
            None => {
                return failure_message(
                    request.context,
                    AuthenticationError::JWKHasNoAlgorithm,
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        let mut validation = Validation::new(algorithm);
        validation.validate_nbf = true;

        let token_data = match decode::<serde_json::Value>(jwt, &decoding_key, &validation) {
            Ok(v) => v,
            Err(e) => {
                return failure_message(
                    request.context,
                    AuthenticationError::CannotDecodeJWT(e),
                    StatusCode::UNAUTHORIZED,
                );
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
        return Ok(ControlFlow::Continue(request));
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

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("apollo", "authentication", AuthenticationPlugin);
