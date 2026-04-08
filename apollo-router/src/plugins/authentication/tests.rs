use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::handler::HandlerWithoutStateExt;
use base64::Engine as _;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;
use http_body_util::BodyExt;
use insta::assert_yaml_snapshot;
use jsonwebtoken::Algorithm;
use jsonwebtoken::EncodingKey;
use jsonwebtoken::encode;
use jsonwebtoken::get_current_timestamp;
use jsonwebtoken::jwk::AlgorithmParameters;
use jsonwebtoken::jwk::CommonParameters;
use jsonwebtoken::jwk::EllipticCurve;
use jsonwebtoken::jwk::EllipticCurveKeyParameters;
use jsonwebtoken::jwk::EllipticCurveKeyType;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::jwk::KeyAlgorithm;
use jsonwebtoken::jwk::KeyOperations;
use jsonwebtoken::jwk::PublicKeyUse;
use mime::APPLICATION_JSON;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::rand_core::OsRng;
use p256::pkcs8::EncodePrivateKey;
use serde::Deserialize;
use serde::Serialize;
use tower::ServiceExt;
use tracing::subscriber;
use url::Url;

use super::APOLLO_AUTHENTICATION_JWT_CLAIMS;
use super::HEADER_TOKEN_TRUNCATED;
use super::Header;
use super::JWT_CONTEXT_KEY;
use super::JWTConf;
use super::JwtStatus;
use super::Source;
use super::authenticate;
use crate::assert_errors_eq_ignoring_id;
use crate::assert_response_eq_ignoring_error_id;
use crate::assert_snapshot_subscriber;
use crate::graphql;
use crate::plugin::test;
use crate::plugins::authentication::Issuers;
use crate::plugins::authentication::jwks::Audiences;
use crate::plugins::authentication::jwks::JWTCriteria;
use crate::plugins::authentication::jwks::JwksConfig;
use crate::plugins::authentication::jwks::JwksManager;
use crate::plugins::authentication::jwks::SearchResult;
use crate::plugins::authentication::jwks::parse_jwks;
use crate::plugins::authentication::jwks::search_jwks;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::services::supergraph;

pub(crate) fn create_an_url(filename: &str) -> String {
    let jwks_base = Path::new("tests");

    let jwks_path = jwks_base.join("fixtures").join(filename);

    let jwks_absolute_path = std::fs::canonicalize(jwks_path).unwrap();

    Url::from_file_path(jwks_absolute_path).unwrap().to_string()
}

async fn build_a_default_test_harness() -> router::BoxCloneService {
    build_a_test_harness(None, None, false, false, false).await
}

async fn build_a_test_harness(
    header_name: Option<String>,
    header_value_prefix: Option<String>,
    multiple_jwks: bool,
    ignore_other_prefixes: bool,
    continue_on_error: bool,
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
                "router": {
                    "jwt": {
                        "jwks": [
                            {
                                "url": &jwks_url
                            },
                            {
                                "url": &jwks_url
                            }
                        ]
                    }
                }
            }
        })
    } else {
        serde_json::json!({
            "authentication": {
                "router": {
                    "jwt" : {
                        "jwks": [
                            {
                                "url": &jwks_url
                            }
                        ]
                    }
                }
            }
        })
    };

    if let Some(hn) = header_name {
        config["authentication"]["router"]["jwt"]["header_name"] = serde_json::Value::String(hn);
    }

    if let Some(hp) = header_value_prefix {
        config["authentication"]["router"]["jwt"]["header_value_prefix"] =
            serde_json::Value::String(hp);
    }

    config["authentication"]["router"]["jwt"]["ignore_other_prefixes"] =
        serde_json::Value::Bool(ignore_other_prefixes);

    if continue_on_error {
        config["authentication"]["router"]["jwt"]["on_error"] =
            serde_json::Value::String("Continue".to_string());
    }

    match crate::TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .supergraph_hook(move |_| mock_service.clone().boxed_clone())
        .build_router()
        .await
    {
        Ok(test_harness) => test_harness,
        Err(e) => panic!("Failed to build test harness: {e}"),
    }
}

#[tokio::test]
async fn load_plugin() {
    let _test_harness = build_a_default_test_harness().await;
}

#[tokio::test]
async fn it_rejects_when_there_is_no_auth_header() {
    let mut mock_service = test::MockSupergraphService::new();
    mock_service.expect_clone().return_once(move || {
        println!("cloned to supergraph mock");
        let mut mock_service = test::MockSupergraphService::new();
        mock_service.expect_call().never();
        mock_service
    });
    let jwks_url = create_an_url("jwks.json");

    let config = serde_json::json!({
        "authentication": {
            "router": {
                "jwt" : {
                    "jwks": [
                        {
                            "url": &jwks_url
                        }
                    ]
                }
            }
        },
        "rhai": {
            "scripts":"tests/fixtures",
            "main":"require_authentication.rhai"
        }
    });
    let test_harness = crate::TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .supergraph_hook(move |_| mock_service.clone().boxed_clone())
        .build_router()
        .await
        .unwrap();

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder().build().unwrap();

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
        .message("The request is not authenticated")
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());
}

#[tokio::test]
async fn it_rejects_when_auth_prefix_is_missing() {
    let test_harness = build_a_default_test_harness().await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
        .message(format!(
            "Value of '{0}' JWT header should be prefixed with 'Bearer'",
            http::header::AUTHORIZATION,
        ))
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
}

#[tokio::test]
async fn it_rejects_when_auth_prefix_has_no_jwt_token() {
    let test_harness = build_a_default_test_harness().await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
        .message(format!(
            "Value of '{0}' JWT header has only 'Bearer' prefix but no JWT token",
            http::header::AUTHORIZATION,
        ))
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
}

#[tokio::test]
async fn it_rejects_when_auth_prefix_has_invalid_format_jwt() {
    let test_harness = build_a_default_test_harness().await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
        .message(format!(
            "'{HEADER_TOKEN_TRUNCATED}' is not a valid JWT header: InvalidToken"
        ))
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
}

#[tokio::test]
async fn it_rejects_when_auth_prefix_has_correct_format_but_invalid_jwt() {
    let test_harness = build_a_default_test_harness().await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
        .message(format!("'{HEADER_TOKEN_TRUNCATED}' is not a valid JWT header: Base64 error: Invalid last symbol 114, offset 5."))
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::BAD_REQUEST, service_response.response.status());
}

#[tokio::test]
async fn it_rejects_when_auth_prefix_has_correct_format_and_invalid_jwt() {
    let test_harness = build_a_default_test_harness().await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
        .message("Cannot decode JWT: Base64 error: Invalid last symbol 66, offset 42.")
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());
}

#[tokio::test]
async fn it_accepts_when_auth_prefix_has_correct_format_and_valid_jwt() {
    let test_harness = build_a_default_test_harness().await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
async fn it_accepts_when_auth_prefix_does_not_match_config_and_is_ignored() {
    let test_harness = build_a_test_harness(None, None, false, true, false).await;
    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, "Basic dXNlcjpwYXNzd29yZA==")
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
    let test_harness = build_a_test_harness(None, None, true, false, false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
    let test_harness =
        build_a_test_harness(Some("SOMETHING".to_string()), None, false, false, false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
    let test_harness =
        build_a_test_harness(None, Some("SOMETHING".to_string()), false, false, false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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
async fn it_accepts_when_no_auth_prefix_and_valid_jwt_custom_prefix() {
    let test_harness = build_a_test_harness(None, Some("".to_string()), false, false, false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
            .header(
                http::header::AUTHORIZATION,
                "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A",
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
async fn it_inserts_success_jwt_status_into_context() {
    let test_harness = build_a_test_harness(None, None, false, false, false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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

    let jwt_context = service_response
        .context
        .get::<_, JwtStatus>(JWT_CONTEXT_KEY)
        .expect("deserialization succeeds")
        .expect("a context value was set");

    match jwt_context {
        JwtStatus::Success { r#type, name } => {
            assert_eq!(r#type, "header");
            assert!(name.eq_ignore_ascii_case("Authorization"));
        }
        JwtStatus::Failure { .. } => panic!("expected a success but got {jwt_context:?}"),
    }

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

    let jwt_claims = service_response
        .context
        .get::<_, serde_json::Value>(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .expect("deserialization succeeds")
        .expect("a context value was set");

    assert_eq!(
        jwt_claims,
        serde_json::json!({
            "exp": 10_000_000_000i64,
            "another claim": "this is another claim"
        })
    );
}

#[tokio::test]
async fn it_inserts_failure_jwt_status_into_context() {
    let test_harness = build_a_test_harness(None, None, false, false, false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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

    let jwt_context = service_response
        .context
        .get::<_, JwtStatus>(JWT_CONTEXT_KEY)
        .expect("deserialization succeeds")
        .expect("a context value was set");

    let error = jwt_context.error();
    match error {
        Some(err) => {
            assert_eq!(err.code, "CANNOT_DECODE_JWT");
            assert_eq!(
                err.message,
                "Cannot decode JWT: Base64 error: Invalid last symbol 66, offset 42."
            );
        }
        None => panic!("expected an error"),
    }

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
        .message("Cannot decode JWT: Base64 error: Invalid last symbol 66, offset 42.")
        .extension_code("AUTH_ERROR")
        .build();

    assert_errors_eq_ignoring_id!(response.errors, [expected_error]);

    assert_eq!(StatusCode::UNAUTHORIZED, service_response.response.status());

    let jwt_claims = service_response
        .context
        .get::<_, serde_json::Value>(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .expect("deserialization succeeds");

    assert!(
        jwt_claims.is_none(),
        "because the JWT was invalid, no claims should be set"
    );
}

#[tokio::test]
async fn it_moves_on_after_jwt_errors_when_configured() {
    let test_harness = build_a_test_harness(None, None, false, false, true).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
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

    let jwt_context = service_response
        .context
        .get::<_, JwtStatus>(JWT_CONTEXT_KEY)
        .expect("deserialization succeeds")
        .expect("a context value was set");

    let error = jwt_context.error();
    match error {
        Some(err) => {
            assert_eq!(err.code, "CANNOT_DECODE_JWT");
            assert_eq!(
                err.message,
                "Cannot decode JWT: Base64 error: Invalid last symbol 66, offset 42."
            );
        }
        None => panic!("expected an error"),
    }

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

    // JWT decode failure should be ignored
    assert_eq!(response.errors, vec![]);

    assert_eq!(StatusCode::OK, service_response.response.status());

    let jwt_claims = service_response
        .context
        .get::<_, serde_json::Value>(APOLLO_AUTHENTICATION_JWT_CLAIMS)
        .expect("deserialization succeeds");

    assert!(
        jwt_claims.is_none(),
        "because the JWT was invalid, no claims should be set"
    );
}

#[tokio::test]
#[should_panic]
async fn it_panics_when_auth_prefix_has_correct_format_but_contains_whitespace() {
    let _test_harness =
        build_a_test_harness(None, Some("SOMET HING".to_string()), false, false, false).await;
}

#[tokio::test]
#[should_panic]
async fn it_panics_when_auth_prefix_has_correct_format_but_contains_trailing_whitespace() {
    let _test_harness =
        build_a_test_harness(None, Some("SOMETHING ".to_string()), false, false, false).await;
}

#[tokio::test]
async fn it_extracts_the_token_from_cookies() {
    let mut mock_service = test::MockSupergraphService::new();
    mock_service.expect_clone().return_once(move || {
        println!("cloned to supergraph mock");
        let mut mock_service = test::MockSupergraphService::new();
        mock_service
            .expect_call()
            .once()
            .returning(move |req: supergraph::Request| {
                Ok(supergraph::Response::fake_builder()
                    .data("response created within the mock")
                    .context(req.context)
                    .build()
                    .unwrap())
            });
        mock_service
    });
    let jwks_url = create_an_url("jwks.json");

    let config = serde_json::json!({
        "authentication": {
            "router": {
                "jwt" : {
                    "jwks": [
                        {
                            "url": &jwks_url
                        }
                    ],
                    "sources": [
                        {
                            "type": "cookie",
                            "name": "authz"
                        }
                    ],
                }
            }
        },
        "rhai": {
            "scripts":"tests/fixtures",
            "main":"require_authentication.rhai"
        }
    });
    let test_harness = crate::TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .supergraph_hook(move |_| mock_service.clone().boxed_clone())
        .build_router()
        .await
        .unwrap();

    let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A";

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
        .header(
            http::header::COOKIE,
            format!("a= b; c = d HttpOnly; authz = {token}; e = f"),
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
async fn it_supports_multiple_sources() {
    let mut mock_service = test::MockSupergraphService::new();
    mock_service.expect_clone().return_once(move || {
        println!("cloned to supergraph mock");
        let mut mock_service = test::MockSupergraphService::new();
        mock_service
            .expect_call()
            .once()
            .returning(move |req: supergraph::Request| {
                Ok(supergraph::Response::fake_builder()
                    .data("response created within the mock")
                    .context(req.context)
                    .build()
                    .unwrap())
            });
        mock_service
    });
    let jwks_url = create_an_url("jwks.json");

    let config = serde_json::json!({
        "authentication": {
            "router": {
                "jwt" : {
                    "jwks": [
                        {
                            "url": &jwks_url
                        }
                    ],
                    "sources": [
                        {
                            "type": "cookie",
                            "name": "authz"
                        },
                        {
                            "type": "header",
                            "name": "authz1"
                        },
                        {
                            "type": "header",
                            "name": "authz2",
                            "value_prefix": "bear"
                        }
                    ],
                }
            }
        },
        "rhai": {
            "scripts":"tests/fixtures",
            "main":"require_authentication.rhai"
        }
    });
    let test_harness = crate::TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .supergraph_hook(move |_| mock_service.clone().boxed_clone())
        .build_router()
        .await
        .unwrap();

    let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiIsImtpZCI6ImtleTEifQ.eyJleHAiOjEwMDAwMDAwMDAwLCJhbm90aGVyIGNsYWltIjoidGhpcyBpcyBhbm90aGVyIGNsYWltIn0.4GrmfxuUST96cs0YUC0DfLAG218m7vn8fO_ENfXnu5A";

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
        .header("Authz2", format!("Bear {token}"))
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

async fn build_jwks_search_components() -> JwksManager {
    let mut sets = vec![];
    let mut urls = vec![];

    let jwks_url = create_an_url("jwks.json");

    sets.push(jwks_url);

    for s_url in &sets {
        let url: Url = Url::from_str(s_url).expect("created a valid url");
        urls.push(JwksConfig {
            url,
            issuers: None,
            audiences: None,
            algorithms: None,
            poll_interval: Duration::from_secs(60),
            allow_missing_exp: false,
            headers: Vec::new(),
        });
    }

    JwksManager::new(urls).await.unwrap()
}

#[tokio::test]
async fn it_finds_key_with_criteria_kid_and_algorithm() {
    let jwks_manager = build_jwks_search_components().await;

    let criteria = JWTCriteria {
        kid: Some("key2".to_string()),
        alg: Algorithm::HS256,
    };

    let SearchResult { jwk: key, .. } = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(KeyAlgorithm::HS256, key.common.key_algorithm.unwrap());
    assert_eq!("key2", key.common.key_id.unwrap());
}

#[tokio::test]
async fn it_finds_best_matching_key_with_criteria_algorithm() {
    let jwks_manager = build_jwks_search_components().await;

    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::HS256,
    };

    let SearchResult { jwk: key, .. } = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(KeyAlgorithm::HS256, key.common.key_algorithm.unwrap());
    assert_eq!("key1", key.common.key_id.unwrap());
}

#[tokio::test]
async fn it_fails_to_find_key_with_criteria_algorithm_not_in_set() {
    let jwks_manager = build_jwks_search_components().await;

    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::RS512,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_none());
}

#[tokio::test]
async fn it_finds_key_with_criteria_algorithm_ec() {
    let jwks_manager = build_jwks_search_components().await;

    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::ES256,
    };

    let SearchResult { jwk: key, .. } = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(KeyAlgorithm::ES256, key.common.key_algorithm.unwrap());
    assert_eq!(
        "afda85e09a320cf748177874592de64d",
        key.common.key_id.unwrap()
    );
}

#[tokio::test]
async fn it_finds_key_with_criteria_algorithm_rsa() {
    let jwks_manager = build_jwks_search_components().await;

    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::RS256,
    };

    let SearchResult { jwk: key, .. } = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(KeyAlgorithm::RS256, key.common.key_algorithm.unwrap());
    assert_eq!(
        "022516583d56b68faf40260fda72978a",
        key.common.key_id.unwrap()
    );
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
    iss: Option<String>,
    aud: Option<String>,
}

fn make_manager_with_allow_missing_exp(
    jwk: &Jwk,
    issuers: Option<Issuers>,
    audiences: Option<Audiences>,
    allow_missing_exp: bool,
) -> JwksManager {
    let jwks = JwkSet {
        keys: vec![jwk.clone()],
    };

    let url = Url::from_str("file:///jwks.json").unwrap();
    let list = vec![JwksConfig {
        url: url.clone(),
        issuers,
        audiences,
        algorithms: None,
        poll_interval: Duration::from_secs(60),
        allow_missing_exp,
        headers: Vec::new(),
    }];
    let map = HashMap::from([(url, jwks); 1]);

    JwksManager::new_test(list, map)
}

fn make_manager(jwk: &Jwk, issuers: Option<Issuers>, audiences: Option<Audiences>) -> JwksManager {
    make_manager_with_allow_missing_exp(jwk, issuers, audiences, false)
}

#[tokio::test]
async fn issuer_check() {
    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_encoded_point(false);

    let encoding_key = EncodingKey::from_ec_der(&signing_key.to_pkcs8_der().unwrap().to_bytes());

    let jwk = Jwk {
        common: CommonParameters {
            public_key_use: Some(PublicKeyUse::Signature),
            key_operations: Some(vec![KeyOperations::Verify]),
            key_algorithm: Some(KeyAlgorithm::ES256),
            key_id: Some("hello".to_string()),
            ..Default::default()
        },
        algorithm: AlgorithmParameters::EllipticCurve(EllipticCurveKeyParameters {
            key_type: EllipticCurveKeyType::EC,
            curve: EllipticCurve::P256,
            x: BASE64_URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            y: BASE64_URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        }),
    };

    let manager = make_manager(
        &jwk,
        Some(HashSet::from(["hello".to_string(), "goodbye".to_string()])),
        None,
    );

    // No issuer
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            iss: None,
            aud: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    let mut config = JWTConf::default();
    config.sources.push(Source::Header {
        name: super::default_header_name(),
        value_prefix: super::default_header_value_prefix(),
    });
    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(res) => {
            panic!("unexpected response: {res:?}");
        }
        ControlFlow::Continue(req) => {
            println!("got req with issuer check");
            let claims: serde_json::Value = req
                .context
                .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .unwrap()
                .unwrap();
            println!("claims: {claims:?}");
        }
    }

    // Valid issuer
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            iss: Some("hello".to_string()),
            aud: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &router::body::into_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_response_eq_ignoring_error_id!(response, graphql::Response::builder()
        .errors(vec![graphql::Error::builder()
            .extension_code("AUTH_ERROR")
            .message("Invalid issuer: the token's `iss` was 'hallo', but signed with a key from JWKS configured to only accept from 'hello'")
            .build()
        ]).build());
        }
        ControlFlow::Continue(req) => {
            println!("got req with issuer check");
            let claims: serde_json::Value = req
                .context
                .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .unwrap()
                .unwrap();
            println!("claims: {claims:?}");
        }
    }

    // Invalid issuer
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            iss: Some("AAAA".to_string()),
            aud: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &router::body::into_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_response_eq_ignoring_error_id!(response, graphql::Response::builder()
            .errors(vec![graphql::Error::builder()
                .extension_code("AUTH_ERROR")
                .message("Invalid issuer: the token's `iss` was 'AAAA', but signed with a key from JWKS configured to only accept from 'goodbye, hello'")
                .build()]).build());
        }
        ControlFlow::Continue(_) => {
            panic!("issuer check should have failed")
        }
    }

    // no issuer check
    let manager = make_manager(&jwk, None, None);
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            iss: Some("hello".to_string()),
            aud: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &router::body::into_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(response, graphql::Response::builder()
        .errors(vec![graphql::Error::builder().extension_code("AUTH_ERROR").message("Invalid issuer: the token's `iss` was 'AAAA', but signed with a key from JWKS configured to only accept from 'hello'").build()]).build());
        }
        ControlFlow::Continue(req) => {
            println!("got req with issuer check");
            let claims: serde_json::Value = req
                .context
                .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .unwrap()
                .unwrap();
            println!("claims: {claims:?}");
        }
    }
}

#[tokio::test]
async fn audience_check() {
    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_encoded_point(false);

    let encoding_key = EncodingKey::from_ec_der(&signing_key.to_pkcs8_der().unwrap().to_bytes());

    let jwk = Jwk {
        common: CommonParameters {
            public_key_use: Some(PublicKeyUse::Signature),
            key_operations: Some(vec![KeyOperations::Verify]),
            key_algorithm: Some(KeyAlgorithm::ES256),
            key_id: Some("hello".to_string()),
            ..Default::default()
        },
        algorithm: AlgorithmParameters::EllipticCurve(EllipticCurveKeyParameters {
            key_type: EllipticCurveKeyType::EC,
            curve: EllipticCurve::P256,
            x: BASE64_URL_SAFE_NO_PAD.encode(point.x().unwrap()),
            y: BASE64_URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        }),
    };

    let manager = make_manager(
        &jwk,
        None,
        Some(HashSet::from(["hello".to_string(), "goodbye".to_string()])),
    );

    // No audience
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            aud: None,
            iss: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    let mut config = JWTConf::default();
    config.sources.push(Source::Header {
        name: super::default_header_name(),
        value_prefix: super::default_header_value_prefix(),
    });
    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(res) => {
            assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
            let body = res.response.into_body().collect().await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body.to_bytes()).unwrap();
            let expected_body = serde_json::json!({
                "errors": [
                    {
                        "message": "Invalid audience: the token's `aud` was 'null', but 'goodbye, hello' was expected",
                        "extensions": {
                            "code": "AUTH_ERROR"
                        }
                    }
                ]
            });
            assert_eq!(body, expected_body);
        }
        ControlFlow::Continue(_req) => {
            panic!("expected a rejection for a lack of audience");
        }
    }

    // Valid audience
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            aud: Some("hello".to_string()),
            iss: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(_res) => {
            panic!("expected audience to be valid");
        }
        ControlFlow::Continue(req) => {
            let claims: serde_json::Value = req
                .context
                .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .unwrap()
                .unwrap();

            assert_eq!(claims["aud"], "hello");
        }
    }

    // Invalid audience
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            aud: Some("AAAA".to_string()),
            iss: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &router::body::into_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_response_eq_ignoring_error_id!(response, graphql::Response::builder()
                .errors(vec![
                    graphql::Error::builder()
                        .extension_code("AUTH_ERROR")
                        .message("Invalid audience: the token's `aud` was 'AAAA', but 'goodbye, hello' was expected")
                        .build()
                ]).build());
        }
        ControlFlow::Continue(_) => {
            panic!("audience check should have failed")
        }
    }

    // no audience check
    let manager = make_manager(&jwk, None, None);
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            aud: Some("hello".to_string()),
            iss: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&config, &manager, request.try_into().unwrap()) {
        ControlFlow::Break(_res) => {
            panic!("expected audience to be valid");
        }
        ControlFlow::Continue(req) => {
            let claims: serde_json::Value = req
                .context
                .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .unwrap()
                .unwrap();
            assert_eq!(claims["aud"], "hello");
        }
    }
}

#[tokio::test]
async fn it_rejects_key_with_restricted_algorithm() {
    let mut sets = vec![];
    let mut urls = vec![];

    let jwks_url = create_an_url("jwks.json");

    sets.push(jwks_url);

    for s_url in &sets {
        let url: Url = Url::from_str(s_url).expect("created a valid url");
        urls.push(JwksConfig {
            url,
            issuers: None,
            audiences: None,
            algorithms: Some(HashSet::from([Algorithm::RS256])),
            poll_interval: Duration::from_secs(60),
            allow_missing_exp: false,
            headers: Vec::new(),
        });
    }

    let jwks_manager = JwksManager::new(urls).await.unwrap();

    // the JWT contains a HMAC key but we configured a restriction to RSA signing
    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::HS256,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_none());
}

#[tokio::test]
async fn it_rejects_and_accepts_keys_with_restricted_algorithms_and_unknown_jwks_algorithm() {
    let mut sets = vec![];
    let mut urls = vec![];

    // Use a jwks which contains an algorithm (ES512) which jsonwebtoken doesn't support
    let jwks_url = create_an_url("jwks-unknown-alg.json");

    sets.push(jwks_url);

    for s_url in &sets {
        let url: Url = Url::from_str(s_url).expect("created a valid url");
        urls.push(JwksConfig {
            url,
            issuers: None,
            audiences: None,
            algorithms: Some(HashSet::from([Algorithm::RS256])),
            poll_interval: Duration::from_secs(60),
            allow_missing_exp: false,
            headers: Vec::new(),
        });
    }

    let jwks_manager = JwksManager::new(urls).await.unwrap();

    // the JWT contains a HMAC key, but we configured a restriction to RSA signing
    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::HS256,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_none());

    // the JWT contains a RSA key (configured to allow)
    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::RS256,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_some());
}

#[tokio::test]
async fn it_accepts_key_without_use_or_keyops() {
    let mut sets = vec![];
    let mut urls = vec![];

    let jwks_url = create_an_url("jwks-no-use.json");

    sets.push(jwks_url);

    for s_url in &sets {
        let url: Url = Url::from_str(s_url).expect("created a valid url");
        urls.push(JwksConfig {
            url,
            issuers: None,
            audiences: None,
            algorithms: None,
            poll_interval: Duration::from_secs(60),
            allow_missing_exp: false,
            headers: Vec::new(),
        });
    }

    let jwks_manager = JwksManager::new(urls).await.unwrap();

    // the JWT contains a HMAC key but we configured a restriction to RSA signing
    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::ES256,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_some());
}

#[tokio::test]
async fn it_accepts_elliptic_curve_key_without_alg() {
    let mut sets = vec![];
    let mut urls = vec![];

    let jwks_url = create_an_url("jwks-ec-no-alg.json");

    sets.push(jwks_url);

    for s_url in &sets {
        let url: Url = Url::from_str(s_url).expect("created a valid url");
        urls.push(JwksConfig {
            url,
            issuers: None,
            audiences: None,
            algorithms: None,
            poll_interval: Duration::from_secs(60),
            allow_missing_exp: false,
            headers: Vec::new(),
        });
    }

    let jwks_manager = JwksManager::new(urls).await.unwrap();

    // the JWT contains a HMAC key but we configured a restriction to RSA signing
    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::ES256,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_some());
}

#[tokio::test]
async fn it_accepts_rsa_key_without_alg() {
    let mut sets = vec![];
    let mut urls = vec![];

    let jwks_url = create_an_url("jwks-rsa-no-alg.json");

    sets.push(jwks_url);

    for s_url in &sets {
        let url: Url = Url::from_str(s_url).expect("created a valid url");
        urls.push(JwksConfig {
            url,
            issuers: None,
            audiences: None,
            algorithms: None,
            poll_interval: Duration::from_secs(60),
            allow_missing_exp: false,
            headers: Vec::new(),
        });
    }

    let jwks_manager = JwksManager::new(urls).await.unwrap();

    // the JWT contains a HMAC key but we configured a restriction to RSA signing
    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::RS384,
    };

    assert!(search_jwks(&jwks_manager, &criteria).is_some());
}

#[test]
fn test_parse_failure_logs() {
    subscriber::with_default(assert_snapshot_subscriber!(), || {
        let jwks = parse_jwks(include_str!("testdata/jwks.json")).expect("expected to parse jwks");
        assert_yaml_snapshot!(jwks);
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn jwks_send_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();

    let got_header = Arc::new(AtomicBool::new(false));
    let gh = got_header.clone();
    let service = move |headers: HeaderMap| {
        println!("got re: {headers:?}");
        let gh: Arc<AtomicBool> = gh.clone();
        async move {
            if headers.get("jwks-authz").and_then(|v| v.to_str().ok()) == Some("user1") {
                gh.store(true, Ordering::Release);
            }
            http::Response::builder()
                .header(http::header::CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .status(StatusCode::OK)
                .version(http::Version::HTTP_11)
                .body::<RouterBody>(router::body::from_bytes(include_str!("testdata/jwks.json")))
                .unwrap()
        }
    };
    let server = axum::serve(listener, service.into_make_service());
    tokio::task::spawn(async { server.await.unwrap() });

    let url = Url::parse(&format!("http://{socket_addr}/")).unwrap();

    let _jwks_manager = JwksManager::new(vec![JwksConfig {
        url,
        issuers: None,
        audiences: None,
        algorithms: Some(HashSet::from([Algorithm::RS256])),
        poll_interval: Duration::from_secs(60),
        allow_missing_exp: false,
        headers: vec![Header {
            name: HeaderName::from_static("jwks-authz"),
            value: HeaderValue::from_static("user1"),
        }],
    }])
    .await
    .unwrap();

    assert!(got_header.load(Ordering::Acquire));
}

mod common {
    use base64::Engine as _;
    use base64::prelude::BASE64_URL_SAFE_NO_PAD;
    use jsonwebtoken::Algorithm;
    use jsonwebtoken::EncodingKey;
    use jsonwebtoken::encode;
    use jsonwebtoken::jwk::AlgorithmParameters;
    use jsonwebtoken::jwk::CommonParameters;
    use jsonwebtoken::jwk::EllipticCurve;
    use jsonwebtoken::jwk::EllipticCurveKeyParameters;
    use jsonwebtoken::jwk::EllipticCurveKeyType;
    use jsonwebtoken::jwk::Jwk;
    use jsonwebtoken::jwk::KeyAlgorithm;
    use jsonwebtoken::jwk::KeyOperations;
    use jsonwebtoken::jwk::PublicKeyUse;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePrivateKey;

    use crate::plugins::authentication::JWTConf;
    use crate::plugins::authentication::Source;
    use crate::plugins::authentication::default_header_name;
    use crate::plugins::authentication::default_header_value_prefix;
    use crate::services::router;
    use crate::services::supergraph;

    pub(super) fn jwk(signing_key: &SigningKey) -> Jwk {
        jwk_with_kid(signing_key, "hello")
    }

    pub(super) fn jwk_with_kid(signing_key: &SigningKey, kid: &str) -> Jwk {
        let verifying_key = signing_key.verifying_key();
        let point = verifying_key.to_encoded_point(false);
        Jwk {
            common: CommonParameters {
                public_key_use: Some(PublicKeyUse::Signature),
                key_operations: Some(vec![KeyOperations::Verify]),
                key_algorithm: Some(KeyAlgorithm::ES256),
                key_id: Some(kid.to_string()),
                ..Default::default()
            },
            algorithm: AlgorithmParameters::EllipticCurve(EllipticCurveKeyParameters {
                key_type: EllipticCurveKeyType::EC,
                curve: EllipticCurve::P256,
                x: BASE64_URL_SAFE_NO_PAD.encode(point.x().unwrap()),
                y: BASE64_URL_SAFE_NO_PAD.encode(point.y().unwrap()),
            }),
        }
    }

    fn encoding_key(signing_key: &SigningKey) -> EncodingKey {
        EncodingKey::from_ec_der(&signing_key.to_pkcs8_der().unwrap().to_bytes())
    }

    pub(super) fn jwt_conf_with_header_source() -> JWTConf {
        let mut config = JWTConf::default();
        config.sources.push(Source::Header {
            name: default_header_name(),
            value_prefix: default_header_value_prefix(),
        });
        config
    }

    pub(super) fn build_request_with_header_token(
        signing_key: SigningKey,
        token_claims: serde_json::Value,
    ) -> router::Request {
        let token = encode(
            &jsonwebtoken::Header::new(Algorithm::ES256),
            &token_claims,
            &encoding_key(&signing_key),
        )
        .unwrap();

        supergraph::Request::canned_builder()
            .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
            .build()
            .unwrap()
            .try_into()
            .unwrap()
    }

    pub(super) fn build_request_with_header_token_kid(
        signing_key: SigningKey,
        token_claims: serde_json::Value,
        kid: &str,
    ) -> router::Request {
        let mut header = jsonwebtoken::Header::new(Algorithm::ES256);
        header.kid = Some(kid.to_string());
        let token = encode(&header, &token_claims, &encoding_key(&signing_key)).unwrap();

        supergraph::Request::canned_builder()
            .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
            .build()
            .unwrap()
            .try_into()
            .unwrap()
    }
}

mod expiry_validation {
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::ops::ControlFlow;
    use std::str::FromStr;
    use std::time::Duration;

    use http::StatusCode;
    use jsonwebtoken::get_current_timestamp;
    use jsonwebtoken::jwk::JwkSet;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::rand_core::OsRng;
    use url::Url;

    use super::common::build_request_with_header_token;
    use super::common::build_request_with_header_token_kid;
    use super::common::jwk;
    use super::common::jwk_with_kid;
    use super::common::jwt_conf_with_header_source;
    use super::make_manager;
    use super::make_manager_with_allow_missing_exp;
    use crate::plugins::authentication::authenticate;
    use crate::plugins::authentication::jwks::Audiences;
    use crate::plugins::authentication::jwks::Issuers;
    use crate::plugins::authentication::jwks::JwksConfig;
    use crate::plugins::authentication::jwks::JwksManager;
    use crate::services::router;

    fn authenticate_request(
        token_claims: serde_json::Value,
        allow_missing_exp: bool,
    ) -> ControlFlow<router::Response, router::Request> {
        let signing_key = SigningKey::random(&mut OsRng);
        let manager = if allow_missing_exp {
            make_manager_with_allow_missing_exp(&jwk(&signing_key), None, None, true)
        } else {
            make_manager(&jwk(&signing_key), None, None)
        };

        let request = build_request_with_header_token(signing_key, token_claims);
        authenticate(&jwt_conf_with_header_source(), &manager, request)
    }

    #[test]
    fn it_rejects_jwt_without_exp_by_default() {
        let token_claims = serde_json::json!({
            "sub": "test"
        });

        match authenticate_request(token_claims, false) {
            ControlFlow::Break(response) => {
                assert_eq!(response.response.status(), StatusCode::UNAUTHORIZED);
            }
            ControlFlow::Continue(_) => {
                panic!("request without `exp` should be rejected by default");
            }
        }
    }

    #[test]
    fn it_accepts_jwt_without_exp_when_configured() {
        let token_claims = serde_json::json!({
            "sub": "test"
        });

        match authenticate_request(token_claims, true) {
            ControlFlow::Continue(_) => {}
            ControlFlow::Break(response) => {
                panic!(
                    "request without `exp` should be accepted when allow_missing_exp is enabled: {response:?}"
                );
            }
        }
    }

    #[test]
    fn it_rejects_jwt_with_expired_exp_when_configured() {
        let token_claims = serde_json::json!({
            "sub": "test",
            "exp": 1
        });

        match authenticate_request(token_claims, true) {
            ControlFlow::Break(response) => {
                assert_eq!(response.response.status(), StatusCode::UNAUTHORIZED);
            }
            ControlFlow::Continue(_) => {
                panic!(
                    "request with expired `exp` should be rejected even when allow_missing_exp is enabled"
                );
            }
        }
    }

    #[test]
    fn it_accepts_jwt_with_future_exp_when_configured() {
        let token_claims = serde_json::json!({
            "sub": "test",
            "exp": get_current_timestamp() + 3600
        });

        match authenticate_request(token_claims, true) {
            ControlFlow::Continue(_) => {}
            ControlFlow::Break(response) => {
                panic!(
                    "request with future `exp` should be accepted when allow_missing_exp is enabled: {response:?}"
                );
            }
        }
    }

    fn authenticate_request_with_constraints(
        token_claims: serde_json::Value,
        issuers: Option<Issuers>,
        audiences: Option<Audiences>,
        allow_missing_exp: bool,
    ) -> ControlFlow<router::Response, router::Request> {
        let signing_key = SigningKey::random(&mut OsRng);
        let manager = make_manager_with_allow_missing_exp(
            &jwk(&signing_key),
            issuers,
            audiences,
            allow_missing_exp,
        );
        let request = build_request_with_header_token(signing_key, token_claims);
        authenticate(&jwt_conf_with_header_source(), &manager, request)
    }

    #[test]
    fn it_rejects_jwt_without_exp_with_wrong_issuer() {
        let issuers = HashSet::from(["https://expected.example.com".to_string()]);
        let token_claims = serde_json::json!({
            "sub": "test",
            "iss": "https://wrong.example.com"
        });

        match authenticate_request_with_constraints(token_claims, Some(issuers), None, true) {
            ControlFlow::Break(response) => {
                assert_eq!(
                    response.response.status(),
                    StatusCode::INTERNAL_SERVER_ERROR
                );
            }
            ControlFlow::Continue(_) => {
                panic!("token with wrong issuer must be rejected even with allow_missing_exp");
            }
        }
    }

    #[test]
    fn it_rejects_jwt_without_exp_with_wrong_audience() {
        let audiences = HashSet::from(["expected-audience".to_string()]);
        let token_claims = serde_json::json!({
            "sub": "test",
            "aud": "wrong-audience"
        });

        match authenticate_request_with_constraints(token_claims, None, Some(audiences), true) {
            ControlFlow::Break(response) => {
                assert_eq!(response.response.status(), StatusCode::UNAUTHORIZED);
            }
            ControlFlow::Continue(_) => {
                panic!("token with wrong audience must be rejected even with allow_missing_exp");
            }
        }
    }

    #[test]
    fn it_respects_per_jwks_allow_missing_exp_scoping() {
        // JWKS A: allow_missing_exp = true, kid = "key-a"
        let signing_key_a = SigningKey::random(&mut OsRng);
        let jwk_a = jwk_with_kid(&signing_key_a, "key-a");

        // JWKS B: allow_missing_exp = false, kid = "key-b"
        let signing_key_b = SigningKey::random(&mut OsRng);
        let jwk_b = jwk_with_kid(&signing_key_b, "key-b");

        let url_a = Url::from_str("file:///jwks-a.json").unwrap();
        let url_b = Url::from_str("file:///jwks-b.json").unwrap();

        let list = vec![
            JwksConfig {
                url: url_a.clone(),
                issuers: None,
                audiences: None,
                algorithms: None,
                poll_interval: Duration::from_secs(60),
                allow_missing_exp: true,
                headers: Vec::new(),
            },
            JwksConfig {
                url: url_b.clone(),
                issuers: None,
                audiences: None,
                algorithms: None,
                poll_interval: Duration::from_secs(60),
                allow_missing_exp: false,
                headers: Vec::new(),
            },
        ];

        let map = HashMap::from([
            (url_a, JwkSet { keys: vec![jwk_a] }),
            (url_b, JwkSet { keys: vec![jwk_b] }),
        ]);

        let manager = JwksManager::new_test(list, map);

        // Token signed by key A (allow_missing_exp=true), no exp -> must accept
        let token_claims = serde_json::json!({"sub": "test"});
        let request = build_request_with_header_token_kid(signing_key_a, token_claims, "key-a");
        match authenticate(&jwt_conf_with_header_source(), &manager, request) {
            ControlFlow::Continue(_) => {}
            ControlFlow::Break(response) => {
                panic!(
                    "token without exp signed by key-a (allow_missing_exp=true) should be accepted: {response:?}"
                );
            }
        }

        // Token signed by key B (allow_missing_exp=false), no exp -> must reject
        let token_claims = serde_json::json!({"sub": "test"});
        let request = build_request_with_header_token_kid(signing_key_b, token_claims, "key-b");
        match authenticate(&jwt_conf_with_header_source(), &manager, request) {
            ControlFlow::Break(response) => {
                assert_eq!(response.response.status(), StatusCode::UNAUTHORIZED);
            }
            ControlFlow::Continue(_) => {
                panic!(
                    "token without exp must be rejected when matched JWKS has allow_missing_exp=false"
                );
            }
        }
    }
}

mod audience_validation {
    use std::ops::ControlFlow;

    use http::StatusCode;
    use jsonwebtoken::get_current_timestamp;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::rand_core::OsRng;

    use super::common::build_request_with_header_token;
    use super::common::jwk;
    use super::common::jwt_conf_with_header_source;
    use super::make_manager;
    use crate::plugins::authentication::authenticate;
    use crate::services::router;

    fn authenticate_request(
        manager_aud: &[&str],
        token_aud: serde_json::Value,
    ) -> ControlFlow<router::Response, router::Request> {
        let signing_key = SigningKey::random(&mut OsRng);
        let manager_audiences = if manager_aud.is_empty() {
            None
        } else {
            Some(manager_aud.iter().map(ToString::to_string).collect())
        };
        let manager = make_manager(&jwk(&signing_key), None, manager_audiences);

        let token_claims = serde_json::json!({
            "sub": "test",
            "exp": get_current_timestamp(),
            "aud": token_aud
        });

        let request = build_request_with_header_token(signing_key, token_claims);
        authenticate(&jwt_conf_with_header_source(), &manager, request)
    }

    #[rstest::rstest]
    #[case::multiple_auds(&["hello", "world"], serde_json::json!(["hello", "world"]))]
    #[case::multiple_with_array_accepted(&["hello", "world", "goodbye"], serde_json::json!(["hello"]))]
    #[case::multiple_with_str_accepted(&["hello", "world"], serde_json::json!("hello"))]
    #[case::multiple_with_str_accepted(&["hello", "world"], serde_json::json!("world"))]
    #[case::single_with_array_accepted_any_of(&["hello"], serde_json::json!(["hello", "world"]))]
    #[case::single_with_array_accepted(&["hello"], serde_json::json!(["hello"]))]
    #[case::single_aud(&["hello"], serde_json::json!("hello"))]
    #[case::multiple_with_single_intersection(&["hello", "world", "goodbye"], serde_json::json!(["hola", "bonjour", "hello"]))]
    #[case::null_mgr_aud(&[], serde_json::Value::Null)]
    #[case::null_mgr_aud_with_token_aud(&[], serde_json::json!("hello"))]
    fn it_accepts_jwt(#[case] manager_aud: &[&str], #[case] token_aud: serde_json::Value) {
        match authenticate_request(manager_aud, token_aud.clone()) {
            ControlFlow::Continue(_) => {}
            ControlFlow::Break(response) => {
                panic!(
                    "Request should be permitted: manager_aud = {manager_aud:?}, token_aud = {token_aud}, response = {response:?}"
                );
            }
        }
    }

    #[rstest::rstest]
    #[case::missing_token_aud(&["hello", "world"], serde_json::json!(""))]
    #[case::mismatched_single_aud(&["hello"], serde_json::json!("world"))]
    #[case::mismatched_single_aud_array(&["hello"], serde_json::json!(["world", "planet"]))]
    #[case::mismatched_multiple_aud(&["hello", "world"], serde_json::json!(["goodbye", "planet"]))]
    fn it_rejects_jwt(#[case] manager_aud: &[&str], #[case] token_aud: serde_json::Value) {
        match authenticate_request(manager_aud, token_aud.clone()) {
            ControlFlow::Continue(_) => {
                panic!(
                    "Request should be rejected: manager_aud = {manager_aud:?}, token_aud = {token_aud}"
                );
            }
            ControlFlow::Break(response) => {
                assert_eq!(
                    response.response.status(),
                    StatusCode::UNAUTHORIZED,
                    "manager_aud = {manager_aud:?}, token_aud = {token_aud}, response = {response:?}"
                );
            }
        }
    }
}

mod issuer_validation {
    use std::ops::ControlFlow;

    use http::StatusCode;
    use jsonwebtoken::get_current_timestamp;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::rand_core::OsRng;

    use super::common::build_request_with_header_token;
    use super::common::jwk;
    use super::common::jwt_conf_with_header_source;
    use super::make_manager;
    use crate::plugins::authentication::authenticate;
    use crate::services::router;

    fn authenticate_request(
        manager_iss: &[&str],
        token_iss: serde_json::Value,
    ) -> ControlFlow<router::Response, router::Request> {
        let signing_key = SigningKey::random(&mut OsRng);

        let manager_issuers = if manager_iss.is_empty() {
            None
        } else {
            Some(manager_iss.iter().map(ToString::to_string).collect())
        };
        let manager = make_manager(&jwk(&signing_key), manager_issuers, None);

        let token_claims = serde_json::json!({
            "sub": "test",
            "exp": get_current_timestamp(),
            "iss": token_iss
        });

        let request = build_request_with_header_token(signing_key, token_claims);
        authenticate(&jwt_conf_with_header_source(), &manager, request)
    }

    #[rstest::rstest]
    #[case::multiple_with_str_accepted(&["hello", "world"], serde_json::json!("hello"))]
    #[case::multiple_with_str_accepted(&["hello", "world"], serde_json::json!("world"))]
    #[case::single_iss(&["hello"], serde_json::json!("hello"))]
    #[case::null_mgr_iss_with_token_iss(&[], serde_json::json!("hello"))]
    #[case::null_mgr_iss_with_empty_token_iss(&[], serde_json::Value::Null)]
    #[case::empty_token_iss(&["hello", "world"], serde_json::Value::Null)]
    #[case::empty_token_iss(&["hello"], serde_json::Value::Null)]
    fn it_accepts_jwt(#[case] manager_iss: &[&str], #[case] token_iss: serde_json::Value) {
        match authenticate_request(manager_iss, token_iss.clone()) {
            ControlFlow::Continue(_) => {}
            ControlFlow::Break(response) => {
                panic!(
                    "Request should be permitted: manager_iss = {manager_iss:?}, token_iss = {token_iss}, response = {response:?}"
                );
            }
        }
    }

    #[rstest::rstest]
    #[case::multiple_iss(&["hello", "world"], serde_json::json!(["hello", "world"]))]
    #[case::multiple_with_array_accepted(&["hello", "world", "goodbye"], serde_json::json!(["hello"]))]
    #[case::single_with_array_accepted_any_of(&["hello"], serde_json::json!(["hello", "world"]))]
    #[case::single_with_array_accepted(&["hello"], serde_json::json!(["hello"]))]
    #[case::missing_token_iss(&["hello", "world"], serde_json::json!(""))]
    #[case::mismatched_single_iss(&["hello"], serde_json::json!("world"))]
    #[case::mismatched_single_iss_array(&["hello"], serde_json::json!(["world"]))]
    #[case::mismatched_single_iss_array(&["hello"], serde_json::json!(["world", "planet"]))]
    #[case::mismatched_multiple_iss(&["hello", "world"], serde_json::json!(["goodbye", "planet"]))]
    fn it_rejects_jwt(#[case] manager_iss: &[&str], #[case] token_iss: serde_json::Value) {
        match authenticate_request(manager_iss, token_iss.clone()) {
            ControlFlow::Continue(_) => {
                panic!(
                    "Request should be rejected: manager_iss = {manager_iss:?}, token_iss = {token_iss}"
                );
            }
            ControlFlow::Break(response) => {
                assert_eq!(
                    response.response.status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "manager_iss = {manager_iss:?}, token_iss = {token_iss}, response = {response:?}"
                );
            }
        }
    }
}
