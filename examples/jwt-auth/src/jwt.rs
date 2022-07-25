//! Example implementation of a JWT verifying plugin
//!
//! DISCLAIMER:
//!     This is an example for illustrative purposes. It has not been security audited
//!     and is purely intended of an illustration of an approach to JWT verification
//!     via a router plugin.
//!
//! The plugin uses [`jwt_simple`](https://crates.io/crates/jwt-simple)
//!
//! The plugin provides support for HMAC algorithms. Additional algorithms (RSA, etc..)
//! are supported by the crate, but not implemented in this plugin (yet...)
//!
//! Usage:
//!
//! In your router.yaml, specify the following details:
//! ```yaml
//! plugins:
//! Authentication Mechanism
//!   # Must configure:
//!   #  - algorithm: HS256 | HS384 | HS512
//!   #  - key: valid base64 encoded key
//!   #
//!   example.jwt:
//!     algorithm: HS256
//!     key: 629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba
//! ```
//! algorithm: your choice of verifying HMAC algorithm
//! key: hex encoded key
//!
//! There are also two optional parameters
//! time_tolerance: <u64>
//! max_token_life: <u64>
//!
//! Both of these parameters are in units of seconds. Both default to 15 mins if
//! not specified
//! time_tolerance: is how much time we are prepared to add on to expiration dates
//! max_token_life: is the time between issued and expires for a token
//!
//! The time_tolerance exists to accommodate variations in clock timings between systems.
//!
//! max_token_life is enforced to prevent use of long lived (and possibly compromised)
//! tokens through this plugin.
//!
//! Verification Limitations:
//!
//! The plugin enforces token expiry (as modified by time_tolerance and max_token_life)
//! and then updates the context of the query to store the verified claims in the
//! query context under the Key: "JWTClaims". Look at test
//!     test_hmac_jwtauth_accepts_valid_tokens()
//! for an example of how the claims are propagated into the request and how they can
//! be examined in a downstream service.
//!
//! Limitations:
//!
//! This plugin is purely for purposes of illustration. A production plugin would include
//! many more features not available here:
//!  - Multiple verification mechanisms
//!  - Additional algorithms
//!  - Custom Claims
//!  - Token refresh
//!  - ...

use std::ops::ControlFlow;
use std::str::FromStr;

use apollo_router::graphql;
use apollo_router::layers::ServiceBuilderExt;
use apollo_router::plugin::Plugin;
use apollo_router::register_plugin;
use apollo_router::services::RouterRequest;
use apollo_router::services::RouterResponse;
use apollo_router::Context;
use http::header::AUTHORIZATION;
use http::StatusCode;
use jwt_simple::prelude::*;
use jwt_simple::Error;
use schemars::JsonSchema;
use serde::de;
use serde::Deserialize;
use strum_macros::EnumString;
use tower::util::BoxService;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

// It's a shame that we can't just have one enum which finds the algorithm and contains
// the verifier, but the verification structs don't support Default, so we can't
// use EnumString to match our algorithms if we try. Choosing convenience of algorithm
// detection here with sligh inconvenience of requiring two enums.
#[derive(Clone, Debug, EnumString)]
enum HMACAlgorithm {
    HS256,
    HS384,
    HS512,
}

// Although there are traits for the various families of algorithms, e.g.: MACLike,
// they aren't object safe, so we can't box up our verifier and save it in our
// JwtHmac struct. We'll use an enum instead.
#[derive(Clone, Debug)]
enum HMACVerifier {
    HS256(HS256Key),
    HS384(HS384Key),
    HS512(HS512Key),
}

// Simple hand-off our verification process. The verification is generic over
// custom claims. In our code we aren't using any, but this implementation
// supports them.
impl HMACVerifier {
    fn verify_token<CustomClaims: Serialize + de::DeserializeOwned>(
        &self,
        token: &str,
        options: Option<VerificationOptions>,
    ) -> Result<JWTClaims<CustomClaims>, Error> {
        match self {
            HMACVerifier::HS256(verifier) => verifier.verify_token(token, options),
            HMACVerifier::HS384(verifier) => verifier.verify_token(token, options),
            HMACVerifier::HS512(verifier) => verifier.verify_token(token, options),
        }
    }
}

// We are storing the algorithm, but not using it. Hence the allow dead code.
#[derive(Debug)]
#[allow(dead_code)]
struct JwtHmac {
    // HMAC algorithm in use
    algorithm: HMACAlgorithm,
    // Actual verifier with key
    verifier: HMACVerifier,
}

// We are storing the configuration, but not using it. Hence the allow dead code.
#[derive(Debug, Default)]
#[allow(dead_code)]
struct JwtAuth {
    // Store our configuration in case we need it later
    configuration: Conf,
    // Time tolerance for token verification
    time_tolerance: Duration,
    // Maximum token life
    max_token_life: Duration,
    // HMAC may be configured
    hmac: Option<JwtHmac>,
    // Add support for additional algorithms here
}

impl JwtAuth {
    const DEFAULT_MAX_TOKEN_LIFE: u64 = 900;

    // HMAC support
    fn try_initialize_hmac(configuration: &Conf, key: String) -> Option<JwtHmac> {
        let mut hmac = None;
        let hmac_algorithm = HMACAlgorithm::from_str(configuration.algorithm.as_str()).ok();

        if let Some(algorithm) = hmac_algorithm {
            let verifier = match algorithm {
                HMACAlgorithm::HS256 => {
                    HMACVerifier::HS256(HS256Key::from_bytes(hex::decode(key).ok()?.as_ref()))
                }
                HMACAlgorithm::HS384 => {
                    HMACVerifier::HS384(HS384Key::from_bytes(hex::decode(key).ok()?.as_ref()))
                }
                HMACAlgorithm::HS512 => {
                    HMACVerifier::HS512(HS512Key::from_bytes(hex::decode(key).ok()?.as_ref()))
                }
            };
            hmac = Some(JwtHmac {
                algorithm,
                verifier,
            });
        }
        hmac
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct Conf {
    algorithm: String,
    key: String,
    time_tolerance: Option<u64>,
    max_token_life: Option<u64>,
}

#[async_trait::async_trait]
impl Plugin for JwtAuth {
    type Config = Conf;

    async fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        // Try to figure out which authentication mechanism to use
        let key = configuration.key.trim().to_string();

        let time_tolerance = match configuration.time_tolerance {
            Some(t) => Duration::from_secs(t),
            None => Duration::from_secs(DEFAULT_TIME_TOLERANCE_SECS),
        };
        let max_token_life = match configuration.max_token_life {
            Some(t) => Duration::from_secs(t),
            None => Duration::from_secs(JwtAuth::DEFAULT_MAX_TOKEN_LIFE),
        };
        let hmac = JwtAuth::try_initialize_hmac(&configuration, key);

        Ok(Self {
            time_tolerance,
            max_token_life,
            configuration,
            hmac,
        })
    }

    fn router_service(
        &self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // We are going to use the `jwt-simple` crate for our JWT verification.
        // The crate provides straightforward support for the popular JWT algorithms.

        // `ServiceBuilder` provides us with an `checkpoint` method.
        //
        // This method allows us to return ControlFlow::Continue(request) if we want to let the request through,
        // or ControlFlow::Break(response) with a crafted response if we don't want the request to go through.

        // Clone/Copy the data we need in our closure.
        let mut hmac_verifier = None;
        if let Some(hmac) = &self.hmac {
            hmac_verifier = Some(hmac.verifier.clone());
        }
        let time_tolerance = self.time_tolerance;
        let max_token_life = self.max_token_life;

        ServiceBuilder::new()
            .checkpoint(move |req: RouterRequest| {
                // We are going to do a lot of similar checking so let's define a local function
                // to help reduce repetition
                fn failure_message(
                    context: Context,
                    msg: String,
                    status: StatusCode,
                ) -> Result<ControlFlow<RouterResponse, RouterRequest>, BoxError> {
                    let res = RouterResponse::error_builder()
                        .errors(vec![graphql::Error {
                            message: msg,
                            ..Default::default()
                        }])
                        .status_code(status)
                        .context(context)
                        .build()?;
                    Ok(ControlFlow::Break(res))
                }

                // The http_request is stored in a `RouterRequest` context.
                // We are going to check the headers for the presence of the header we're looking for
                // We are implementing: https://www.rfc-editor.org/rfc/rfc6750
                // so check for our AUTHORIZATION header.
                let jwt_value_result = match req.originating_request.headers().get(AUTHORIZATION) {
                    Some(value) => value.to_str(),
                    None =>
                        // Prepare an HTTP 401 response with a GraphQL error message
                        return failure_message(req.context, format!("Missing '{}' header", AUTHORIZATION), StatusCode::UNAUTHORIZED),
                };

                // If we find the header, but can't convert it to a string, let the client know
                let jwt_value_untrimmed = match jwt_value_result {
                    Ok(value) => value,
                    Err(_not_a_string_error) => {
                        // Prepare an HTTP 400 response with a GraphQL error message
                        return failure_message(req.context,
                                               "AUTHORIZATION' header is not convertible to a string".to_string(),
                            StatusCode::BAD_REQUEST,
                        );
                    }
                };

                // Let's trim out leading and trailing whitespace to be accommodating
                let jwt_value = jwt_value_untrimmed.trim();

                // Make sure the format of our message matches our expectations
                // Technically, the spec is case sensitive, but let's accept
                // case variations
                if !jwt_value.to_uppercase().as_str().starts_with("BEARER ") {
                    // Prepare an HTTP 400 response with a GraphQL error message
                    return failure_message(req.context,
                                           format!("'{jwt_value_untrimmed}' is not correctly formatted"),
                        StatusCode::BAD_REQUEST,
                    );
                }

                // We know we have a "space", since we checked above. Split our string
                // in (at most 2) sections.
                let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                if jwt_parts.len() != 2 {
                    // Prepare an HTTP 400 response with a GraphQL error message
                    return failure_message(req.context,
                                           format!("'{jwt_value}' is not correctly formatted"),
                        StatusCode::BAD_REQUEST,
                    );
                }

                // Trim off any trailing white space (not valid in BASE64 encoding)
                let jwt = jwt_parts[1].trim_end();

                // Now let's try to validate our token
                let options = VerificationOptions { time_tolerance: Some(time_tolerance), ..Default::default() };
                if let Some(verifier) = &hmac_verifier {
                    match verifier.verify_token::<JWTClaims<NoCustomClaims>>(
                        jwt,
                        Some(options),
                    ) {
                        Ok(claims) => {
                            // Our JWT is basically valid, but, let's make sure it wasn't issued
                            // with a lifetime greater than we can tolerate.
                            match claims.expires_at {
                                Some(expires) => {
                                    match claims.issued_at {
                                        Some(issued) => {
                                            if expires - issued > max_token_life {
                                                // Prepare an HTTP 403 response with a GraphQL error message
                                                return failure_message(req.context,
                                                                       format!("{jwt} is not authorized: expiry period exceeds policy limit"),
                                                    StatusCode::FORBIDDEN,
                                                );
                                            }
                                        },
                                        None => {
                                            // Prepare an HTTP 403 response with a GraphQL error message
                                            return failure_message(req.context,
                                                                   format!("{jwt} is not authorized: no issue time set"),
                                                StatusCode::FORBIDDEN,
                                            );
                                        }
                                    }
                                },
                                None => {
                                    // Prepare an HTTP 403 response with a GraphQL error message
                                    return failure_message(req.context,
                                                           format!("{jwt} is not authorized: no expiry time set"),
                                        StatusCode::FORBIDDEN,
                                    );
                                }
                            }
                            // We are happy with this JWT, on we go...
                            // Let's put our claims in the context and then continue
                            match req.context.insert("JWTClaims", claims) {
                                Ok(_v) => Ok(ControlFlow::Continue(req)),
                                Err(err) => {
                                    return failure_message(req.context,
                                                           format!("couldn't store JWT claims in context: {}", err),
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                    );
                                }
                            }
                        },
                        Err(err) => {
                            // Prepare an HTTP 403 response with a GraphQL error message
                            return failure_message(req.context,
                                                   format!("{jwt} is not authorized: {}", err),
                                StatusCode::FORBIDDEN,
                            );
                        }
                    }
                } else {
                    // We've only implemented support for HMAC. Three possibilities here:
                    //  - Either we tried to specify a valid, but unimplemented algorithm.
                    //  - Or we have specified an invalid algorithm (typo?).
                    //  - Or we haven't configured hmac support.
                    failure_message(req.context,
                                    "Only hmac support is implemented. Check configuration for typos".to_string(),
                        StatusCode::NOT_IMPLEMENTED,
                    )
                }
            })
            .service(service)
            .boxed()
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("example", "jwt", JwtAuth);

// Writing plugins means writing tests that make sure they behave as expected!
//
// apollo_router provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use apollo_router::graphql;
    use apollo_router::plugin::test;
    use apollo_router::plugin::Plugin;
    use apollo_router::services::RouterRequest;
    use apollo_router::services::RouterResponse;

    use super::*;

    // This test ensures the router will be able to
    // find our `JwtAuth` plugin,
    // and deserialize an hmac configured yml configuration into it
    // see `router.yaml` for more information
    #[tokio::test]
    async fn plugin_registered() {
        apollo_router::plugin::plugins()
            .get("example.jwt")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "algorithm": "HS256" , "key": "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_no_authorization_header() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request without an authorization header
        let request_without_any_authorization_header = RouterRequest::fake_builder()
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_without_any_authorization_header)
            .await
            .unwrap();

        // JwtAuth should return a 401...
        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "Missing 'authorization' header".to_string(),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_authorization_header_should_start_with_bearer() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request with a badly formatted authorization header
        let request_with_no_bearer_in_auth = RouterRequest::fake_builder()
            .header("authorization", "should start with Bearer")
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_no_bearer_in_auth)
            .await
            .unwrap();

        // JwtAuth should return a 400...
        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "'should start with Bearer' is not correctly formatted",
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_authorization_header_should_start_with_bearer_with_one_space() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request with a badly formatted authorization header
        let request_with_too_many_spaces_in_auth = RouterRequest::fake_builder()
            .header("authorization", "Bearer  ")
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_too_many_spaces_in_auth)
            .await
            .unwrap();

        // JwtAuth should return a 400...
        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "'Bearer  ' is not correctly formatted",
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_default_jwtauth_requires_at_least_hmac_configuration() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request with a properly formatted authorization header
        // Note: (The token isn't valid, but the format is...)
        let request_with_appropriate_auth = RouterRequest::fake_builder()
            .header("authorization", "Bearer atoken")
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 501...
        assert_eq!(
            StatusCode::NOT_IMPLEMENTED,
            service_response.response.status()
        );

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            "Only hmac support is implemented. Check configuration for typos",
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_hmac_jwtauth_accepts_valid_tokens() {
        // create a mock service we will use to test our plugin
        let mut mock = test::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock.expect_call()
            .once()
            .returning(move |req: RouterRequest| {
                // Let's make sure our request contains (some of) our JWTClaims
                let claims: JWTClaims<NoCustomClaims> = req
                    .context
                    .get::<String, JWTClaims<NoCustomClaims>>("JWTClaims".to_string())
                    .expect("claims are present") // Result
                    .expect("really, they are present"); // Option
                assert_eq!(claims.issuer, Some("issuer".to_string()));
                assert_eq!(claims.subject, Some("subject".to_string()));
                assert_eq!(claims.jwt_id, Some("jwt_id".to_string()));
                assert_eq!(claims.nonce, Some("nonce".to_string()));
                Ok(RouterResponse::fake_builder()
                    .data(expected_mock_response_data)
                    .build()
                    .expect("expecting valid request"))
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        // Create valid configuration for testing HMAC algorithm HS256
        let key = "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba";
        let conf: Conf = serde_json::from_value(serde_json::json!({
            "algorithm": "HS256".to_string(),
            "key": key.to_string(),
        }))
        .expect("json must be valid");

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let jwt_auth = JwtAuth::new(conf)
            .await
            .expect("valid configuration should succeed");

        let service_stack = jwt_auth.router_service(mock_service.boxed());

        let verifier = HS256Key::from_bytes(hex::decode(key).unwrap().as_ref());
        let mut audiences = HashSet::new();
        audiences.insert("audience 1".to_string());
        audiences.insert("audience 2".to_string());
        let claims = Claims::create(Duration::from_mins(2))
            .with_issuer("issuer")
            .with_subject("subject")
            .with_audiences(audiences)
            .with_jwt_id("jwt_id")
            .with_nonce("nonce");
        let token = verifier.authenticate(claims).unwrap();

        // Let's create a request with a properly formatted authorization header
        let request_with_appropriate_auth = RouterRequest::fake_builder()
            .header("authorization", &format!("Bearer {token}"))
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert!(graphql_response.errors.is_empty());
        assert_eq!(expected_mock_response_data, graphql_response.data.unwrap())
    }

    #[tokio::test]
    async fn test_hmac_jwtauth_does_not_accept_long_lived_tokens() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // Create valid configuration for testing HMAC algorithm HS256
        let key = "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba";
        let conf: Conf = serde_json::from_value(serde_json::json!({
            "algorithm": "HS256",
            "key": key,
            "max_token_life": 60,
        }))
        .expect("json must be valid");
        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let jwt_auth = JwtAuth::new(conf)
            .await
            .expect("valid configuration should succeed");

        let service_stack = jwt_auth.router_service(mock_service.boxed());

        let verifier = HS256Key::from_bytes(hex::decode(key).unwrap().as_ref());
        // Generate a token which has an overly generous life span
        let claims = Claims::create(Duration::from_secs(61));
        let token = verifier.authenticate(claims).unwrap();

        // Let's create a request with a properly formatted authorization header
        let request_with_appropriate_auth = RouterRequest::fake_builder()
            .header("authorization", format!("Bearer {token}"))
            .build()
            .expect("expecting valid request");

        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 403...
        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            format!("{token} is not authorized: expiry period exceeds policy limit"),
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_hmac_jwtauth_does_not_accept_expired_tokens() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = test::MockRouterService::new().build();

        // Create valid configuration for testing HMAC algorithm HS256
        let key = "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba";
        let tolerance = 0;
        let conf: Conf = serde_json::from_value(serde_json::json!({
            "algorithm": "HS256".to_string(),
            "key": key.to_string(),
            "time_tolerance": tolerance,
        }))
        .expect("json must be valid");

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let jwt_auth = JwtAuth::new(conf)
            .await
            .expect("valid configuration should succeed");

        let service_stack = jwt_auth.router_service(mock_service.boxed());

        let verifier = HS256Key::from_bytes(hex::decode(key).unwrap().as_ref());
        // Generate a token which has a short life span
        let token_life = 1;
        let claims = Claims::create(Duration::from_secs(token_life));
        let token = verifier.authenticate(claims).unwrap();

        // Let's create a request with a properly formatted authorization header
        let request_with_appropriate_auth = RouterRequest::fake_builder()
            .header("authorization", format!("Bearer {token}"))
            .build()
            .expect("expecting valid request");

        // Let's sleep until our token has expired
        tokio::time::sleep(tokio::time::Duration::from_secs(tolerance + token_life + 1)).await;
        // ...And call our service stack with it
        let mut service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 403...
        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());

        // with the expected error message
        let graphql_response: graphql::Response = service_response.next_response().await.unwrap();

        assert_eq!(
            format!("{token} is not authorized: Token has expired"),
            graphql_response.errors[0].message
        )
    }
}
