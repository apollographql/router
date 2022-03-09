use apollo_router_core::{
    checkpoint::Step, plugin_utils, register_plugin, Plugin, RouterRequest, RouterResponse,
    ServiceBuilderExt,
};
use http::header::AUTHORIZATION;
use http::StatusCode;
use jwt_simple::prelude::*;
use jwt_simple::Error;
use schemars::JsonSchema;
use serde::de;
use serde::Deserialize;
use std::str::FromStr;
use strum_macros::EnumString;
use tower::{util::BoxService, BoxError, ServiceBuilder, ServiceExt};

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
    // HMAC may be configured
    hmac: Option<JwtHmac>,
    // Add support for additional algorithms here
}

impl JwtAuth {
    fn new(configuration: Conf) -> Result<Self, BoxError> {
        // Try to figure out which authentication mechanism to use
        let key = configuration.key.trim().to_string();

        let hmac = JwtAuth::try_initialize_hmac(&configuration, key);

        Ok(Self {
            configuration,
            hmac,
        })
    }

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
}

#[async_trait::async_trait]
impl Plugin for JwtAuth {
    type Config = Conf;

    async fn startup(&mut self) -> Result<(), BoxError> {
        tracing::debug!("starting: {}: {}", stringify!(JwtAuth), self.name());
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), BoxError> {
        tracing::debug!("shutting down: {}: {}", stringify!(JwtAuth), self.name());
        Ok(())
    }

    fn router_service(
        &mut self,
        service: BoxService<RouterRequest, RouterResponse, BoxError>,
    ) -> BoxService<RouterRequest, RouterResponse, BoxError> {
        // We are going to use the `jwt-simple` crate for our JWT verification.
        // The crate provides straightforward support for the popular JWT algorithms.

        // `ServiceBuilder` provides us with an `checkpoint` method.
        //
        // This method allows us to return Step::Continue(request) if we want to let the request through,
        // or Step::Return(response) with a crafted response if we don't want the request to go through.

        // Clone the data we need in our closure.
        let mut hmac_verifier = None;
        if let Some(hmac) = &self.hmac {
            hmac_verifier = Some(hmac.verifier.clone());
        }

        ServiceBuilder::new()
            .checkpoint(move |req: RouterRequest| {
                // We are going to do a lot of similar checking so let's define a local function
                // to help reduce repetition
                fn failure_message(
                    msg: String,
                    status: StatusCode,
                ) -> Result<Step<RouterRequest, RouterResponse>, BoxError> {
                    let res = plugin_utils::RouterResponse::builder()
                        .errors(vec![apollo_router_core::Error {
                            message: msg,
                            ..Default::default()
                        }])
                        .build()
                        .with_status(status);
                    Ok(Step::Return(res))
                }

                // The http_request is stored in a `RouterRequest` context.
                // We are going to check the headers for the presence of the header we're looking for
                // We are implementing: https://www.rfc-editor.org/rfc/rfc6750
                // so check for our AUTHORIZATION header.
                if !req.context.request.headers().contains_key(AUTHORIZATION) {
                    // Prepare an HTTP 401 response with a GraphQL error message
                    return failure_message(
                        format!("Missing '{}' header", AUTHORIZATION),
                        StatusCode::UNAUTHORIZED,
                    );
                }

                // It is best practice to perform checks before we unwrap,
                // And to use `expect()` instead of `unwrap()`, with a message
                // that explains why the use of `expect()` is safe
                let jwt_value_result = req
                    .context
                    .request
                    .headers()
                    .get(AUTHORIZATION)
                    .expect("this cannot fail; we checked for header presence above;qed")
                    .to_str();

                let jwt_value_untrimmed = match jwt_value_result {
                    Ok(value) => value,
                    Err(_not_a_string_error) => {
                        // Prepare an HTTP 400 response with a GraphQL error message
                        return failure_message(
                            "AUTHORIZATION' header is not convertible to a string".to_string(),
                            StatusCode::BAD_REQUEST,
                        );
                    }
                };

                // Let's trim out leading and trailing whitespace to be accomodating
                let jwt_value = jwt_value_untrimmed.trim();

                // Make sure the format of our message matches our expectations
                // Technically, the spec is case sensitive, but let's accept
                // case variations
                if !jwt_value.to_uppercase().as_str().starts_with("BEARER ") {
                    // Prepare an HTTP 400 response with a GraphQL error message
                    return failure_message(
                        format!("'{jwt_value_untrimmed}' is not correctly formatted"),
                        StatusCode::BAD_REQUEST,
                    );
                }

                // We know we have a "space", since we checked above. Split our string
                // in (at most 2) sections and trim the second section.
                let jwt_parts: Vec<&str> = jwt_value.splitn(2, ' ').collect();
                if jwt_parts.len() != 2 {
                    // Prepare an HTTP 400 response with a GraphQL error message
                    return failure_message(
                        format!("'{jwt_value}' is not correctly formatted"),
                        StatusCode::BAD_REQUEST,
                    );
                }

                // Trim off any trailing white space (not valid in BASE64 encoding)
                let jwt = jwt_parts[1].trim_end();

                // Now let's try to validate our token
                // Default time tolerance is 15 mins. That's perhaps a bit generous,
                // so we'll set that to 5 seconds.
                let options = VerificationOptions { time_tolerance: Some(Duration::from_secs(5)), ..Default::default() };
                if let Some(verifier) = &hmac_verifier {
                    match verifier.verify_token::<NoCustomClaims>(
                        jwt,
                        Some(options),
                    ) {
                        Ok(claims) => {
                            // Our JWT is basically valid, but, let's refuse JWTs that were issued
                            // with a lifetime greater than 15 mins
                            match claims.expires_at {
                                Some(expires) => {
                                    match claims.issued_at {
                                        Some(issued) => {
                                            if expires - issued > Duration::from_mins(15) {
                                                // Prepare an HTTP 403 response with a GraphQL error message
                                                return failure_message(
                                                    format!("{jwt} is not authorized: expiry period exceeds policy limit"),
                                                    StatusCode::FORBIDDEN,
                                                );
                                            }
                                        },
                                        None => {
                                            // Prepare an HTTP 403 response with a GraphQL error message
                                            return failure_message(
                                                format!("{jwt} is not authorized: no issue time set"),
                                                StatusCode::FORBIDDEN,
                                            );
                                        }
                                    }
                                },
                                None => {
                                    // Prepare an HTTP 403 response with a GraphQL error message
                                    return failure_message(
                                        format!("{jwt} is not authorized: no expiry time set"),
                                        StatusCode::FORBIDDEN,
                                    );
                                }
                            }
                            // We are happy with this JWT, on we go...
                            Ok(Step::Continue(req))
                        },
                        Err(err) => {
                            // Prepare an HTTP 403 response with a GraphQL error message
                            return failure_message(
                                format!("{jwt} is not authorized: {}", err),
                                StatusCode::FORBIDDEN,
                            );
                        }
                    }
                } else {
                    // We've only implemented support for HMAC. Two possibilities here:
                    //  - Either we tried to specify a valid, but unimplemented algorithem
                    //  - Or we have specified an invalid algorithem (typo?). Either way:
                    //  - Or we haven't configured hmac support.
                    failure_message(
                        "Only hmac support is implemented. Check configuration for typos".to_string(),
                        StatusCode::NOT_IMPLEMENTED,
                    )
                }
            })
            .service(service)
            .boxed()
    }

    fn new(configuration: Self::Config) -> Result<Self, BoxError> {
        tracing::debug!("JwtAuth configuration {:?}!", configuration);
        JwtAuth::new(configuration)
    }
}

// This macro allows us to use it in our plugin registry!
// register_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_plugin!("com.example", "jwt", JwtAuth);

// Writing plugins means writing tests that make sure they behave as expected!
//
// apollo_router_core provides a lot of utilities that will allow you to craft requests, responses,
// and test your plugins in isolation:
#[cfg(test)]
mod tests {
    use super::*;
    use apollo_router_core::{plugin_utils, Plugin};

    // This test ensures the router will be able to
    // find our `JwtAuth` plugin,
    // and deserialize an hmac configured yml configuration into it
    // see config.yml for more information
    #[test]
    fn plugin_registered() {
        apollo_router_core::plugins()
            .get("com.example.jwt")
            .expect("Plugin not found")
            .create_instance(&serde_json::json!({ "algorithm": "HS256" , "key": "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba"}))
            .unwrap();
    }

    #[tokio::test]
    async fn test_no_authorization_header() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = plugin_utils::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request without an authorization header
        let request_without_any_authorization_header =
            plugin_utils::RouterRequest::builder().build().into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_without_any_authorization_header)
            .await
            .unwrap();

        // JwtAuth should return a 401...
        assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

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
        let mock_service = plugin_utils::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request with a badly formatted authorization header
        let request_with_no_bearer_in_auth = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "authorization".to_string(),
                "should start with Bearer".to_string(),
            )])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_no_bearer_in_auth)
            .await
            .unwrap();

        // JwtAuth should return a 400...
        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

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
        let mock_service = plugin_utils::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request with a badly formatted authorization header
        let request_with_too_many_spaces_in_auth = plugin_utils::RouterRequest::builder()
            .headers(vec![("authorization".to_string(), "Bearer  ".to_string())])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_too_many_spaces_in_auth)
            .await
            .unwrap();

        // JwtAuth should return a 400...
        assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

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
        let mock_service = plugin_utils::MockRouterService::new().build();

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let service_stack = JwtAuth::default().router_service(mock_service.boxed());

        // Let's create a request with a properly formatted authorization header
        // Note: (The token isn't valid, but the format is...)
        let request_with_appropriate_auth = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "authorization".to_string(),
                "Bearer atoken".to_string(),
            )])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 501...
        assert_eq!(
            StatusCode::NOT_IMPLEMENTED,
            service_response.response.status()
        );

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

        assert_eq!(
            "Only hmac support is implemented. Check configuration for typos",
            graphql_response.errors[0].message
        )
    }

    #[tokio::test]
    async fn test_hmac_jwtauth_accepts_valid_tokens() {
        // create a mock service we will use to test our plugin
        let mut mock = plugin_utils::MockRouterService::new();

        // The expected reply is going to be JSON returned in the RouterResponse { data } section.
        let expected_mock_response_data = "response created within the mock";

        // Let's set up our mock to make sure it will be called once
        mock.expect_call()
            .once()
            .returning(move |_req: RouterRequest| {
                // We don't care about the contents of the request, so ignore it
                // let's return the expected data
                Ok(plugin_utils::RouterResponse::builder()
                    .data(expected_mock_response_data.into())
                    .build()
                    .into())
            });

        // The mock has been set up, we can now build a service from it
        let mock_service = mock.build();

        // Create valid configuration for testing HMAC algorithm HS256
        let key = "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba";
        let conf = Conf {
            algorithm: "HS256".to_string(),
            key: key.to_string(),
        };

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let mut jwt_auth = JwtAuth::new(conf).expect("valid configuration should succeed");

        let service_stack = jwt_auth.router_service(mock_service.boxed());

        let verifier = HS256Key::from_bytes(hex::decode(key).unwrap().as_ref());
        let claims = Claims::create(Duration::from_mins(2));
        let token = verifier.authenticate(claims).unwrap();

        // Let's create a request with a properly formatted authorization header
        let request_with_appropriate_auth = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "authorization".to_string(),
                format!("Bearer {token}"),
            )])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 200...
        assert_eq!(StatusCode::OK, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

        assert!(graphql_response.errors.is_empty());
        assert_eq!(expected_mock_response_data, graphql_response.data)
    }

    #[tokio::test]
    async fn test_hmac_jwtauth_does_not_accept_long_lived_tokens() {
        // create a mock service we will use to test our plugin
        // It does not have any behavior, because we do not expect it to be called.
        // If it is called, the test will panic,
        // letting us know JwtAuth did not behave as expected.
        let mock_service = plugin_utils::MockRouterService::new().build();

        // Create valid configuration for testing HMAC algorithm HS256
        let key = "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba";
        let conf = Conf {
            algorithm: "HS256".to_string(),
            key: key.to_string(),
        };

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let mut jwt_auth = JwtAuth::new(conf).expect("valid configuration should succeed");

        let service_stack = jwt_auth.router_service(mock_service.boxed());

        let verifier = HS256Key::from_bytes(hex::decode(key).unwrap().as_ref());
        // Generate a token which has an overly generous life span
        let claims = Claims::create(Duration::from_mins(16));
        let token = verifier.authenticate(claims).unwrap();

        // Let's create a request with a properly formatted authorization header
        let request_with_appropriate_auth = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "authorization".to_string(),
                format!("Bearer {token}"),
            )])
            .build()
            .into();

        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 403...
        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

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
        let mock_service = plugin_utils::MockRouterService::new().build();

        // Create valid configuration for testing HMAC algorithm HS256
        let key = "629709bdc3bd794312ccc3a1c47beb03ac7310bc02d32d4587e59b5ad81c99ba";
        let conf = Conf {
            algorithm: "HS256".to_string(),
            key: key.to_string(),
        };

        // In this service_stack, JwtAuth is `decorating` or `wrapping` our mock_service.
        let mut jwt_auth = JwtAuth::new(conf).expect("valid configuration should succeed");

        let service_stack = jwt_auth.router_service(mock_service.boxed());

        let verifier = HS256Key::from_bytes(hex::decode(key).unwrap().as_ref());
        // Generate a token which has an overly generous life span
        let claims = Claims::create(Duration::from_secs(2));
        let token = verifier.authenticate(claims).unwrap();

        // Let's create a request with a properly formatted authorization header
        let request_with_appropriate_auth = plugin_utils::RouterRequest::builder()
            .headers(vec![(
                "authorization".to_string(),
                format!("Bearer {token}"),
            )])
            .build()
            .into();

        // Let's sleep for 8 seconds, so our token expires
        // Note: We have a 5 second grace period on validation
        tokio::time::sleep(tokio::time::Duration::from_secs(8)).await;
        // ...And call our service stack with it
        let service_response = service_stack
            .oneshot(request_with_appropriate_auth)
            .await
            .unwrap();

        // JwtAuth should return a 403...
        assert_eq!(StatusCode::FORBIDDEN, service_response.response.status());

        // with the expected error message
        let graphql_response: apollo_router_core::Response =
            service_response.response.into_body().try_into().unwrap();

        assert_eq!(
            format!("{token} is not authorized: Token has expired"),
            graphql_response.errors[0].message
        )
    }
}
