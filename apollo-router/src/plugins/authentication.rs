//! Authentication plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use deduplicate::Deduplicate;
use futures::future::BoxFuture;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::StatusCode;
use jsonwebtoken::decode;
use jsonwebtoken::decode_header;
use jsonwebtoken::jwk::AlgorithmParameters;
use jsonwebtoken::jwk::EllipticCurve;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::jwk::KeyOperations;
use jsonwebtoken::jwk::PublicKeyUse;
use jsonwebtoken::Algorithm;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::Validation;
use mime::APPLICATION_JSON;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;
use tokio::fs::read_to_string;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

#[cfg(not(test))]
use crate::error::LicenseError;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
#[cfg(not(test))]
use crate::services::apollo_graph_reference;
use crate::services::router;
use crate::Context;

type SharedDeduplicate =
    Arc<Deduplicate<fn(Url) -> BoxFuture<'static, Option<JwkSet>>, Url, JwkSet>>;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication_plugin";

const DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT: Duration = Duration::from_secs(15);

const DEFAULT_AUTHENTICATION_COOLDOWN: Duration = Duration::from_secs(15);

static COOLDOWN: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| {
    #[cfg(not(test))]
    apollo_graph_reference().ok_or(LicenseError::MissingGraphReference)?;
    Ok(Client::new())
});

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("JWKSet cannot be loaded")]
    JwkSet,

    #[error("header_value_prefix must not contain whitespace")]
    BadHeaderValuePrefix,
}

struct AuthenticationPlugin {
    configuration: JWTConf,
    jwks: SharedDeduplicate,
    jwks_urls: Vec<Url>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct JWTConf {
    /// Retrieve our JWK Sets from these locations
    jwks_urls: Vec<String>,
    /// HTTP header expected to contain JWT
    #[serde(default = "default_header_name")]
    header_name: String,
    /// Header value prefix
    #[serde(default = "default_header_value_prefix")]
    header_value_prefix: String,
    /// JWKS retrieval cooldown
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    cooldown: Option<Duration>,
}

impl Default for JWTConf {
    fn default() -> Self {
        Self {
            jwks_urls: Default::default(),
            header_name: default_header_name(),
            header_value_prefix: default_header_value_prefix(),
            cooldown: Default::default(),
        }
    }
}

// This is temporary. It will be removed when the plugin is promoted
// from experimental.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct ExperimentalConf {
    /// The JWT configuration
    jwt: JWTConf,
}

// We may support additional authentication mechanisms in future, so all
// configuration (which is currently JWT specific) is isolated to the
// JWTConf structure.
/// Authentication
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    /// The experimental configuration
    experimental: ExperimentalConf,
}

fn default_header_name() -> String {
    http::header::AUTHORIZATION.to_string()
}

fn default_header_value_prefix() -> String {
    "Bearer".to_string()
}

fn getter(url: Url) -> BoxFuture<'static, Option<JwkSet>> {
    Box::pin(get_jwks(url))
}

// This function is expected to return an Optional value, but we'd like to let
// users know the various failure conditions. Hence the various clumsy map_err()
// scattered through the processing.
async fn get_jwks(url: Url) -> Option<JwkSet> {
    let data = if url.scheme() == "file" {
        #[cfg(not(test))]
        apollo_graph_reference()
            .ok_or(LicenseError::MissingGraphReference)
            .map_err(|e| {
                tracing::error!(%e, "could not activate authentication feature");
                e
            })
            .ok()?;
        let path = url
            .to_file_path()
            .map_err(|e| {
                tracing::error!("could not process url: {:?}", url);
                e
            })
            .ok()?;
        read_to_string(path)
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not read JWKS path");
                e
            })
            .ok()?
    } else {
        let my_client = CLIENT
            .as_ref()
            .map_err(|e| {
                tracing::error!(%e, "could not activate authentication feature");
                e
            })
            .ok()?
            .clone();

        my_client
            .get(url)
            .header(ACCEPT, APPLICATION_JSON.essence_str())
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .timeout(DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not get url");
                e
            })
            .ok()?
            .text()
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not process url content");
                e
            })
            .ok()?
    };
    let jwks: JwkSet = serde_json::from_str(&data)
        .map_err(|e| {
            tracing::error!(%e, "could not create JWKS from url content");
            e
        })
        .ok()?;
    Some(jwks)
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
async fn search_jwks(
    my_jwks: SharedDeduplicate,
    criteria: &JWTCriteria,
    jwks_urls: Vec<Url>,
    context: &Context,
) -> Result<Option<Jwk>, BoxError> {
    const HIGHEST_SCORE: usize = 2;
    let mut candidates = vec![];
    for jwks_url in jwks_urls {
        context.enter_active_request().await;
        // Get the JWKS here
        let jwks_opt = match my_jwks.get(jwks_url).await {
            Ok(k) => k,
            Err(e) => {
                context.leave_active_request().await;
                return Err(e.into());
            }
        };
        context.leave_active_request().await;
        let jwks = jwks_opt.ok_or(Error::JwkSet)?;
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
                return Ok(Some(key));
            }

            candidates.push((key_score, key));
        }
    }

    tracing::debug!(
        "jwk candidates: {:?}",
        candidates
            .iter()
            .map(|(score, candidate)| (score, &candidate.common.key_id, candidate.common.algorithm))
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
            .experimental
            .jwt
            .header_value_prefix
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            return Err(Error::BadHeaderValuePrefix.into());
        }
        let mut urls = vec![];
        for s_url in &init.config.experimental.jwt.jwks_urls {
            let url: Url = Url::from_str(s_url)?;
            urls.push(url);
        }

        // We have to help the compiler out a bit by casting our function item to be a function
        // pointer.
        let g_f = getter as fn(url::Url) -> Pin<Box<dyn Future<Output = Option<JwkSet>> + Send>>;
        let deduplicator = Deduplicate::with_capacity(g_f, urls.len());

        tracing::info!(jwks_urls=?init.config.experimental.jwt.jwks_urls, "JWT authentication using JWKSets from these");

        Ok(AuthenticationPlugin {
            configuration: init.config.experimental.jwt,
            jwks: Arc::new(deduplicator),
            jwks_urls: urls,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let request_full_config = self.configuration.clone();
        let request_jwks = self.jwks.clone();
        let request_jwks_urls = self.jwks_urls.clone();

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
            .checkpoint_async(move |request: router::Request| {
                let my_config = request_full_config.clone();
                let my_jwks = request_jwks.clone();
                let my_jwks_urls = request_jwks_urls.clone();
                const AUTHENTICATION_KIND: &str = "JWT";

                async move {
                    // We are going to do a lot of similar checking so let's define a local function
                    // to help reduce repetition
                    fn failure_message(
                        context: Context,
                        msg: String,
                        status: StatusCode,
                    ) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
                    {
                        // This is a metric and will not appear in the logs
                        tracing::info!(
                            monotonic_counter.apollo_authentication_failure_count = 1u64,
                            kind = %AUTHENTICATION_KIND
                        );
                        tracing::info!(message = %msg, "jwt authentication failure");
                        let response = router::Response::error_builder()
                            .error(
                                graphql::Error::builder()
                                    .message(msg)
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
                    let jwt_value_result =
                        match request.router_request.headers().get(&my_config.header_name) {
                            Some(value) => value.to_str(),
                            None => {
                                return failure_message(
                                    request.context,
                                    format!("Missing {} header", &my_config.header_name),
                                    StatusCode::UNAUTHORIZED,
                                );
                            }
                        };

                    // If we find the header, but can't convert it to a string, let the client know
                    let jwt_value_untrimmed = match jwt_value_result {
                        Ok(value) => value,
                        Err(_not_a_string_error) => {
                            return failure_message(
                                request.context,
                                "configured header is not convertible to a string".to_string(),
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
                    let prefix_len = my_config.header_value_prefix.len();
                    if jwt_value.len() < prefix_len ||
                      !&jwt_value[..prefix_len]
                        .eq_ignore_ascii_case(&my_config.header_value_prefix)
                    {
                        return failure_message(
                            request.context,
                            format!(
                                "Header Value: '{jwt_value_untrimmed}' is not correctly formatted. prefix should be '{}'",
                                my_config.header_value_prefix
                            ),
                            StatusCode::BAD_REQUEST,
                        );
                    }

                    // Split our string in (at most 2) sections.
                    let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                    if jwt_parts.len() != 2 {
                        return failure_message(
                            request.context,
                            format!("Header Value: '{jwt_value}' is not correctly formatted. Missing JWT"),
                            StatusCode::BAD_REQUEST,
                        );
                    }

                    // We have our jwt
                    let jwt = jwt_parts[1];

                    // Try to create a valid header to work with
                    let jwt_header = match decode_header(jwt) {
                        Ok(h) => h,
                        Err(e) => {
                            return failure_message(
                                request.context,
                                format!("'{jwt}' is not a valid JWT header: {e}"),
                                StatusCode::BAD_REQUEST,
                            );
                        }
                    };

                    // Extract our search criteria from our jwt
                    let criteria = JWTCriteria {
                        kid: jwt_header.kid,
                        alg: jwt_header.alg,
                    };

                    // Search our set of JWKS to find the kid and process it
                    // Note: This will search through JWKS in the order in which they are defined
                    // in configuration.

                    let jwk_opt = match search_jwks(my_jwks.clone(), &criteria, my_jwks_urls, &request.context).await {
                        Ok(j) => j,
                        Err(e) => {
                            return failure_message(
                                request.context,
                                format!("Could not retrieve JWKS set: {e}"),
                                StatusCode::INTERNAL_SERVER_ERROR,
                            );
                        }
                    };

                    if let Some(jwk) = jwk_opt {
                        let decoding_key = match DecodingKey::from_jwk(&jwk) {
                            Ok(k) => k,
                            Err(e) => {
                                return failure_message(
                                    request.context,
                                    format!("Could not create decoding key: {e}"),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                );
                            }
                        };

                        let algorithm = match jwk.common.algorithm {
                            Some(a) => a,
                            None => {
                                return failure_message(
                                    request.context,
                                    "Jwk does not contain an algorithm".to_string(),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                );
                            }
                        };

                        let validation = Validation::new(algorithm);

                        let token_data = match decode::<serde_json::Value>(
                            jwt,
                            &decoding_key,
                            &validation,
                        ) {
                            Ok(v) => v,
                            Err(e) => {
                                return failure_message(
                                    request.context,
                                    format!("Could not create decode JWT: {e}"),
                                    StatusCode::UNAUTHORIZED,
                                );
                            }
                        };

                        if let Err(e) = request
                            .context
                            .insert("apollo_authentication::JWT::claims", token_data.claims)
                        {
                            return failure_message(
                                request.context,
                                format!("Could not insert claims into context: {e}"),
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
                    //
                    // Only perform a potential COOLDOWN if the JWT had a kid.
                    //
                    // We will observe the COOLDOWN, if one is set, to minimise the impact
                    // of DOS attacks via this vector.
                    //
                    // If there is no COOLDOWN, we'll trigger a cache update and set a
                    // COOLDOWN.
                    if criteria.kid.is_some() {
                        if COOLDOWN.load(Ordering::SeqCst) {
                            // This is a metric and will not appear in the logs
                            tracing::info!(monotonic_counter.apollo_authentication_cooldown_count = 1u64, kind = %AUTHENTICATION_KIND);
                            let response = router::Response::error_builder()
                                .error(
                                    graphql::Error::builder()
                                        .message(
                                            "Could not retrieve JWKS set: router cooling down",
                                        )
                                        .extension_code("AUTH_ERROR")
                                        .build(),
                                )
                                .header(
                                    http::header::RETRY_AFTER,
                                    my_config
                                        .cooldown
                                        .unwrap_or(DEFAULT_AUTHENTICATION_COOLDOWN)
                                        .as_secs()
                                        .to_string(),
                                )
                                .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                .context(request.context)
                                .build()?;
                            Ok(ControlFlow::Break(response))
                        } else {
                            // We don't recognise this "kid". Clear our cache and impose a
                            // COOLDOWN.
                            // The COOLDOWN controls attempts to retrieve based on a new "kid".
                            tracing::info!("Clearing cached JWKS");
                            my_jwks.clear();
                            // Only spawn 1 task to remove the cooldown
                            if COOLDOWN
                                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                                .is_ok()
                            {
                                tokio::spawn(async move {
                                    let t = my_config
                                        .cooldown
                                        .unwrap_or(DEFAULT_AUTHENTICATION_COOLDOWN);
                                    tokio::time::sleep(t).await;
                                    COOLDOWN.store(false, Ordering::SeqCst);
                                });
                            }
                            failure_message(
                                request.context,
                                format!("Could not find kid: '{:?}' in JWKS sets", criteria.kid),
                                StatusCode::UNAUTHORIZED,
                            )
                        }
                    } else {
                        failure_message(
                            request.context,
                            format!("Could not find a suitable key for: alg: '{:?}', kid: '{:?}' in JWKS sets", criteria.alg, criteria.kid),
                            StatusCode::UNAUTHORIZED,
                        )
                    }
                }
            })
            .buffered()
            .service(service)
        .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("apollo", "authentication", AuthenticationPlugin);

#[cfg(test)]
mod tests {

    use std::path::Path;

    use super::*;
    use crate::plugin::test;
    use crate::services::supergraph;

    fn create_an_url(filename: &str) -> String {
        let jwks_base = Path::new("tests");

        let jwks_path = jwks_base.join("fixtures").join(filename);
        #[cfg(target_os = "windows")]
        let mut jwks_file = std::fs::canonicalize(jwks_path).unwrap();
        #[cfg(not(target_os = "windows"))]
        let jwks_file = std::fs::canonicalize(jwks_path).unwrap();

        #[cfg(target_os = "windows")]
        {
            // We need to manipulate our canonicalized file if we are on Windows.
            // We replace windows path separators with posix path separators
            // We also drop the first 3 characters from the path since they will be
            // something like (drive letter may vary) '\\?\C:' and that isn't
            // a valid URI
            let mut file_string = jwks_file.display().to_string();
            file_string = file_string.replace("\\", "/");
            let len = file_string
                .char_indices()
                .nth(3)
                .map_or(0, |(idx, _ch)| idx);
            jwks_file = file_string[len..].into();
        }

        format!("file://{}", jwks_file.display())
    }

    async fn build_a_default_test_harness() -> router::BoxCloneService {
        build_a_test_harness(None, None, false).await
    }

    async fn build_a_test_harness(
        header_name: Option<String>,
        header_value_prefix: Option<String>,
        multiple_jwks: bool,
    ) -> router::BoxCloneService {
        // create a mock service we will use to test our plugin
        let mut mock_service = test::MockSupergraphService::new();

        // The expected reply is going to be JSON returned in the SupergraphResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock_service.expect_clone().return_once(move || {
            let mut mock_service = test::MockSupergraphService::new();
            mock_service
                .expect_call()
                .once()
                .returning(move |req: supergraph::Request| {
                    Ok(supergraph::Response::fake_builder()
                        .data(expected_mock_response_data)
                        .context(req.context)
                        .build()
                        .unwrap())
                });
            mock_service
        });

        let jwks_url = create_an_url("jwks.json");

        let mut config = if multiple_jwks {
            serde_json::json!({
                "authentication": {
                    "experimental" : {
                        "jwt" : {
                            "jwks_urls": [&jwks_url, &jwks_url]
                        }
                    }
                }
            })
        } else {
            serde_json::json!({
                "authentication": {
                    "experimental" : {
                        "jwt" : {
                            "jwks_urls": [&jwks_url]
                        }
                    }
                }
            })
        };

        if let Some(hn) = header_name {
            config["authentication"]["experimental"]["jwt"]["header_name"] =
                serde_json::Value::String(hn);
        }

        if let Some(hp) = header_value_prefix {
            config["authentication"]["experimental"]["jwt"]["header_value_prefix"] =
                serde_json::Value::String(hp);
        }

        crate::TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .supergraph_hook(move |_| mock_service.clone().boxed())
            .build_router()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn load_plugin() {
        let _test_harness = build_a_default_test_harness().await;
    }

    #[tokio::test]
    async fn it_rejects_when_there_is_no_auth_header() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        let expected_error = graphql::Error::builder()
            .message("Missing authorization header")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_is_missing() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(http::header::AUTHORIZATION, "invalid")
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        let expected_error = graphql::Error::builder()
            .message(
                "Header Value: 'invalid' is not correctly formatted. prefix should be 'Bearer'",
            )
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_no_jwt() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(http::header::AUTHORIZATION, "Bearer")
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        let expected_error = graphql::Error::builder()
            .message("Header Value: 'Bearer' is not correctly formatted. Missing JWT")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_invalid_format_jwt() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(http::header::AUTHORIZATION, "Bearer header.payload")
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        let expected_error = graphql::Error::builder()
            .message("'header.payload' is not a valid JWT header: InvalidToken")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_correct_format_but_invalid_jwt() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer header.payload.signature",
            )
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        let expected_error = graphql::Error::builder()
            .message("'header.payload.signature' is not a valid JWT header: Base64 error: Invalid last symbol 114, offset 5.")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_correct_format_and_invalid_jwt() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5B",
            )
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        let expected_error = graphql::Error::builder()
            .message("Could not create decode JWT: InvalidSignature")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());
    }

    #[tokio::test]
    async fn it_accepts_when_auth_prefix_has_correct_format_and_valid_jwt() {
        let test_harness = build_a_default_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A",
            )
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        assert_eq!(response.errors, vec![]);

        assert_eq!(StatusCode::OK, service_response.response.status());

        let expected_mock_response_data = "response created within the mock";
        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }

    #[tokio::test]
    async fn it_accepts_when_auth_prefix_has_correct_format_multiple_jwks_and_valid_jwt() {
        let test_harness = build_a_test_harness(None, None, true).await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A",
            )
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        assert_eq!(response.errors, vec![]);

        assert_eq!(StatusCode::OK, service_response.response.status());

        let expected_mock_response_data = "response created within the mock";
        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }

    #[tokio::test]
    async fn it_accepts_when_auth_prefix_has_correct_format_and_valid_jwt_custom_auth() {
        let test_harness = build_a_test_harness(Some("SOMETHING".to_string()), None, false).await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                "SOMETHING",
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A",
            )
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        assert_eq!(response.errors, vec![]);

        assert_eq!(StatusCode::OK, service_response.response.status());

        let expected_mock_response_data = "response created within the mock";
        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }

    #[tokio::test]
    async fn it_accepts_when_auth_prefix_has_correct_format_and_valid_jwt_custom_prefix() {
        let test_harness = build_a_test_harness(None, Some("SOMETHING".to_string()), false).await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "SOMETHING eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A",
            )
            .build()
            .unwrap();

        // ...And call our service stack with it
        let mut service_response = test_harness
            .oneshot(request_with_appropriate_name.try_into().unwrap())
            .await
            .unwrap();
        let response: graphql::Response = serde_json::from_slice(
            service_response
                .next_response()
                .await
                .unwrap()
                .unwrap()
                .to_vec()
                .as_slice(),
        )
        .unwrap();

        assert_eq!(response.errors, vec![]);

        assert_eq!(StatusCode::OK, service_response.response.status());

        let expected_mock_response_data = "response created within the mock";
        // with the expected message
        assert_eq!(expected_mock_response_data, response.data.as_ref().unwrap());
    }

    #[tokio::test]
    #[should_panic]
    async fn it_panics_when_auth_prefix_has_correct_format_but_contains_whitespace() {
        let _test_harness = build_a_test_harness(None, Some("SOMET HING".to_string()), false).await;
    }

    #[tokio::test]
    #[should_panic]
    async fn it_panics_when_auth_prefix_has_correct_format_but_contains_trailing_whitespace() {
        let _test_harness = build_a_test_harness(None, Some("SOMETHING ".to_string()), false).await;
    }

    fn build_jwks_search_components() -> (SharedDeduplicate, Vec<Url>) {
        let mut sets = vec![];
        let mut urls = vec![];

        let jwks_url = create_an_url("jwks.json");

        sets.push(jwks_url);

        for s_url in &sets {
            let url: Url = Url::from_str(s_url).expect("created a valid url");
            urls.push(url);
        }

        // We have to help the compiler out a bit by casting our function item to be a function
        // pointer.
        let g_f = getter as fn(url::Url) -> Pin<Box<dyn Future<Output = Option<JwkSet>> + Send>>;
        let deduplicator = Arc::new(Deduplicate::with_capacity(g_f, urls.len()));
        (deduplicator, urls)
    }

    #[tokio::test]
    async fn it_finds_key_with_criteria_kid_and_algorithm() {
        let (deduplicator, urls) = build_jwks_search_components();
        let context = Context::new();

        let criteria = JWTCriteria {
            kid: Some("key2".to_string()),
            alg: Algorithm::HS256,
        };

        let key = search_jwks(deduplicator, &criteria, urls, &context)
            .await
            .expect("search worked")
            .expect("found a key");
        assert_eq!(Algorithm::HS256, key.common.algorithm.unwrap());
        assert_eq!("key2", key.common.key_id.unwrap());
    }

    #[tokio::test]
    async fn it_finds_best_matching_key_with_criteria_algorithm() {
        let (deduplicator, urls) = build_jwks_search_components();
        let context = Context::new();

        let criteria = JWTCriteria {
            kid: None,
            alg: Algorithm::HS256,
        };

        let key = search_jwks(deduplicator, &criteria, urls, &context)
            .await
            .expect("search worked")
            .expect("found a key");
        assert_eq!(Algorithm::HS256, key.common.algorithm.unwrap());
        assert_eq!("key1", key.common.key_id.unwrap());
    }

    #[tokio::test]
    async fn it_fails_to_find_key_with_criteria_algorithm_not_in_set() {
        let (deduplicator, urls) = build_jwks_search_components();
        let context = Context::new();

        let criteria = JWTCriteria {
            kid: None,
            alg: Algorithm::RS512,
        };

        assert!(search_jwks(deduplicator, &criteria, urls, &context)
            .await
            .expect("search worked")
            .is_none());
    }

    #[tokio::test]
    async fn it_finds_key_with_criteria_algorithm_ec() {
        let (deduplicator, urls) = build_jwks_search_components();
        let context = Context::new();

        let criteria = JWTCriteria {
            kid: None,
            alg: Algorithm::ES256,
        };

        let key = search_jwks(deduplicator, &criteria, urls, &context)
            .await
            .expect("search worked")
            .expect("found a key");
        assert_eq!(Algorithm::ES256, key.common.algorithm.unwrap());
        assert_eq!(
            "afda85e09a320cf748177874592de64d",
            key.common.key_id.unwrap()
        );
    }

    #[tokio::test]
    async fn it_finds_key_with_criteria_algorithm_rsa() {
        let (deduplicator, urls) = build_jwks_search_components();
        let context = Context::new();

        let criteria = JWTCriteria {
            kid: None,
            alg: Algorithm::RS256,
        };

        let key = search_jwks(deduplicator, &criteria, urls, &context)
            .await
            .expect("search worked")
            .expect("found a key");
        assert_eq!(Algorithm::RS256, key.common.algorithm.unwrap());
        assert_eq!(
            "022516583d56b68faf40260fda72978a",
            key.common.key_id.unwrap()
        );
    }
}
