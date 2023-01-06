//! Authentication plugin
// With regards to ELv2 licensing, this entire file is license key functionality

use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use deduplicate::Deduplicate;
use deduplicate::DeduplicateFuture;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use http::StatusCode;
use jsonwebtoken::decode;
use jsonwebtoken::decode_header;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::DecodingKey;
use jsonwebtoken::EncodingKey;
use jsonwebtoken::Header;
use jsonwebtoken::Validation;
use once_cell::sync::Lazy;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::fs::read_to_string;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;
use url::Url;

use crate::error::LicenseError;
use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::apollo_graph_reference;
use crate::services::apollo_key;
use crate::services::router;
use crate::Context;

type SharedDeduplicate = Arc<
    Deduplicate<Box<dyn Fn(Url) -> DeduplicateFuture<JwkSet> + Send + Sync + 'static>, Url, JwkSet>,
>;

pub(crate) const AUTHENTICATION_SPAN_NAME: &str = "authentication plugin";

const DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT: Duration = Duration::from_secs(15);

const DEFAULT_AUTHENTICATION_COOLDOWN: Duration = Duration::from_secs(15);

static COOLDOWN: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

static CLIENT: Lazy<Result<Client, BoxError>> = Lazy::new(|| {
    apollo_graph_reference().ok_or(LicenseError::MissingGraphReference)?;
    apollo_key().ok_or(LicenseError::MissingKey)?;
    Ok(Client::new())
});

struct AuthenticationPlugin {
    configuration: Conf,
    jwks: SharedDeduplicate,
    jwks_url: Url,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    // Retrieve our JWK Set from here
    jwks_url: String,
    // HTTP header expected to contain JWT
    #[serde(default = "default_header_name")]
    header_name: String,
    // Header prefix
    #[serde(default = "default_header_prefix")]
    header_prefix: String,
    // Key retention policy
    #[serde(default)]
    retain_keys: bool,
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "String", default)]
    cooldown: Option<Duration>,
}

fn default_header_name() -> String {
    http::header::AUTHORIZATION.to_string()
}

fn default_header_prefix() -> String {
    "Bearer".to_string()
}

#[async_trait::async_trait]
impl Plugin for AuthenticationPlugin {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let url: Url = Url::from_str(&init.config.jwks_url)?;
        let getter: Box<dyn Fn(Url) -> DeduplicateFuture<JwkSet> + Send + Sync + 'static> =
            Box::new(|url: Url| -> DeduplicateFuture<JwkSet> {
                let fut = async {
                    let data = if url.scheme() == "file" {
                        // TODO: Uncomment to make this commercial only before devcomplete
                        /*
                        apollo_graph_reference()
                            .ok_or(LicenseError::MissingGraphReference)
                            .ok()?;
                        apollo_key().ok_or(LicenseError::MissingKey).ok()?;
                        */
                        let path = url.to_file_path().ok()?;
                        read_to_string(path).await.ok()?
                    } else {
                        let my_client = CLIENT.as_ref().map_err(|e| e.to_string()).ok()?.clone();

                        my_client
                            .get(url)
                            .header(ACCEPT, "application/json")
                            .header(CONTENT_TYPE, "application/json")
                            .timeout(DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT)
                            .send()
                            .await
                            .ok()?
                            .text()
                            .await
                            .ok()?
                    };
                    let jwks: JwkSet = serde_json::from_str(&data).ok()?;
                    Some(jwks)
                };
                Box::pin(fut)
            });
        let deduplicator = Deduplicate::with_capacity(getter, 1);
        // XXX For debugging, generate a valid JWT...
        let key = "c2VjcmV0Cg==";
        let claims = serde_json::json!( {
            "exp": 10_000_000_000usize,
            "another claim": "this is another claim"
        });
        let header = Header {
            kid: Some("gary".to_string()),
            ..Default::default()
        };
        let tok_maybe =
            jsonwebtoken::encode(&header, &claims, &EncodingKey::from_base64_secret(key)?);
        tracing::info!(?tok_maybe, "use this JWT for testing");

        Ok(AuthenticationPlugin {
            configuration: init.config,
            jwks: Arc::new(deduplicator),
            jwks_url: url,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        let request_full_config = self.configuration.clone();
        let request_jwks = self.jwks.clone();
        let request_jwks_url = self.jwks_url.clone();

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
                let my_jwks_url = request_jwks_url.clone();

                async move {
                    // We are going to do a lot of similar checking so let's define a local function
                    // to help reduce repetition
                    fn failure_message(
                        context: Context,
                        msg: String,
                        status: StatusCode,
                    ) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
                    {
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
                    if !jwt_value
                        .to_uppercase()
                        .as_str()
                        .starts_with(&my_config.header_prefix.to_uppercase())
                    {
                        return failure_message(
                            request.context,
                            format!(
                                "{} is not correctly formatted: {jwt_value_untrimmed}",
                                my_config.header_prefix
                            ),
                            StatusCode::BAD_REQUEST,
                        );
                    }

                    // Split our string in (at most 2) sections.
                    let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                    if jwt_parts.len() != 2 {
                        return failure_message(
                            request.context,
                            format!("{jwt_value} is not correctly formatted"),
                            StatusCode::BAD_REQUEST,
                        );
                    }

                    // Trim off any trailing white space (not valid in BASE64 encoding)
                    let jwt = jwt_parts[1].trim_end();

                    // Try to create a valid header to work with
                    let jwt_header = match decode_header(jwt) {
                        Ok(h) => h,
                        Err(e) => {
                            return failure_message(
                                request.context,
                                format!("{jwt} is not a valid JWT header: {e}"),
                                StatusCode::BAD_REQUEST,
                            );
                        }
                    };

                    // Try to find the kid of the header
                    let kid = match jwt_header.kid {
                        Some(k) => k,
                        None => {
                            return failure_message(
                                request.context,
                                "Missing kid value from JWT header".to_string(),
                                StatusCode::BAD_REQUEST,
                            );
                        }
                    };

                    // Get the JWKS here
                    let closure_jwks = my_jwks.clone();
                    // If we ever find that:
                    //  - we can't retrieve a JWKS (a)
                    //  - the retrieved JWKS is None (b)
                    //  - we have a kid that we don't know about (c)
                    // We need to do some additional processing.  (I've tagged with comments a/b/c below)
                    // In scenario (c), we have to clear the cache, since we've got to be able to
                    // force new keys to be loaded.
                    let err_cleanup = move |force_clear_keys| {
                        // If we are forced to clear the keys or the user has instructed us to clear the
                        // cache, then we should.
                        if force_clear_keys || !my_config.retain_keys {
                            tracing::info!("Clearing cached JWKS");
                            closure_jwks.clear();
                            // Impose the COOLDOWN
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
                        }
                    };

                    let jwks_opt = match my_jwks.get(my_jwks_url).await {
                        Ok(k) => k,
                        Err(e) => {
                            err_cleanup(false); // a.
                            return failure_message(
                                request.context,
                                format!("Could not retrieve JWKS set: {e}"),
                                StatusCode::INTERNAL_SERVER_ERROR, // XXX: Best error?
                            );
                        }
                    };

                    let jwks = match jwks_opt {
                        Some(k) => k,
                        None => {
                            err_cleanup(false); // b.
                            return failure_message(
                                request.context,
                                "Could not find JWKS set at the configured location".to_string(),
                                StatusCode::INTERNAL_SERVER_ERROR,
                            );
                        }
                    };

                    // Now let's try to validate our token
                    match jwks.find(&kid) {
                        Some(jwk) => {
                            let decoding_key = match DecodingKey::from_jwk(jwk) {
                                Ok(k) => k,
                                Err(e) => {
                                    return failure_message(
                                        request.context,
                                        format!("Could not create decoding key: {}", e),
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
                                        format!("Could not create decode JWT: {}", e),
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
                                    format!("Could not insert claims into context: {}", e),
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                );
                            }
                            Ok(ControlFlow::Continue(request))
                        }
                        None => {
                            // If we receive a kid we don't recognise, we should try to refresh our
                            // JWKS set. However, and this is important, we don't want to become a
                            // potential DOS. Let's have a COOLDOWN here to prevent this happening
                            // too frequently.
                            // Only perform err_cleanup(), etc... if COOLDOWN is not active.
                            if COOLDOWN.load(Ordering::SeqCst) {
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
                                err_cleanup(true); // c.
                                failure_message(
                                    request.context,
                                    format!("Could not find kid: {kid} in JWKS set"),
                                    StatusCode::UNAUTHORIZED,
                                )
                            }
                        }
                    }
                }
            })
            .buffer(20_000)
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

    use super::*;
    use crate::plugin::test;
    use crate::services::supergraph;

    async fn build_a_test_harness() -> router::BoxCloneService {
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

        let jwks_file = std::fs::canonicalize("tests/fixtures/hmac.json").unwrap();
        let jwks_url = format!("file://{}", jwks_file.display());
        let config = serde_json::json!({
            "authentication": {
                "jwks_url": &jwks_url
            }
        });

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
        let _test_harness = build_a_test_harness().await;
    }

    #[tokio::test]
    async fn it_rejects_when_there_is_no_auth_header() {
        let test_harness = build_a_test_harness().await;

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
        let test_harness = build_a_test_harness().await;

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
            .message("Bearer is not correctly formatted: invalid")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_no_jwt() {
        let test_harness = build_a_test_harness().await;

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
            .message("Bearer is not correctly formatted")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_invalid_format_jwt() {
        let test_harness = build_a_test_harness().await;

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
            .message("header.payload is not a valid JWT header: InvalidToken")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_correct_format_but_invalid_jwt() {
        let test_harness = build_a_test_harness().await;

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
            .message("header.payload.signature is not a valid JWT header: Base64 error: Invalid last symbol 114, offset 5.")
            .extension_code("AUTH_ERROR")
            .build();

        assert_eq!(response.errors, vec![expected_error]);

        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
    }

    #[tokio::test]
    async fn it_rejects_when_auth_prefix_has_correct_format_and_invalid_jwt() {
        let test_harness = build_a_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImdhcnkifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.I1UG-Cx3dHuSvrJpLA7hYVZutpeh8cawgwjPRAm5zss",
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
        let test_harness = build_a_test_harness().await;

        // Let's create a request with our operation name
        let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
            .header(
                http::header::AUTHORIZATION,
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImdhcnkifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.I1UG-Cx3dHuSvrJpLA7hYVSutpeh8cawgwjPRAm5zss",
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
}
