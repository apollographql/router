use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use jsonwebtoken::encode;
use jsonwebtoken::get_current_timestamp;
use jsonwebtoken::jwk::CommonParameters;
use jsonwebtoken::jwk::EllipticCurveKeyParameters;
use jsonwebtoken::jwk::EllipticCurveKeyType;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::EncodingKey;
use p256::ecdsa::SigningKey;
use p256::pkcs8::EncodePrivateKey;
use rand_core::OsRng;
use serde::Serialize;
use serde_json::Value;

use super::*;
use crate::plugin::test;
use crate::services::supergraph;

fn create_an_url(filename: &str) -> String {
    let jwks_base = Path::new("tests");

    let jwks_path = jwks_base.join("fixtures").join(filename);

    let jwks_absolute_path = std::fs::canonicalize(jwks_path).unwrap();

    Url::from_file_path(jwks_absolute_path).unwrap().to_string()
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
        .supergraph_hook(move |_| mock_service.clone().boxed())
        .build_router()
        .await
        .unwrap();

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
        .message("The request is not authenticated")
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
        .message("Header Value: 'invalid' is not correctly formatted. prefix should be 'Bearer'")
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
        .message(format!(
            "'{HEADER_TOKEN_TRUNCATED}' is not a valid JWT header: InvalidToken"
        ))
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
            .message(format!("'{HEADER_TOKEN_TRUNCATED}' is not a valid JWT header: Base64 error: Invalid last symbol 114, offset 5."))
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
        .message("Cannot decode JWT: InvalidSignature")
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
async fn it_accepts_when_no_auth_prefix_and_valid_jwt_custom_prefix() {
    let test_harness = build_a_test_harness(None, Some("".to_string()), false).await;

    // Let's create a request with our operation name
    let request_with_appropriate_name = supergraph::Request::canned_builder()
            .operation_name("me".to_string())
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
#[should_panic]
async fn it_panics_when_auth_prefix_has_correct_format_but_contains_whitespace() {
    let _test_harness = build_a_test_harness(None, Some("SOMET HING".to_string()), false).await;
}

#[tokio::test]
#[should_panic]
async fn it_panics_when_auth_prefix_has_correct_format_but_contains_trailing_whitespace() {
    let _test_harness = build_a_test_harness(None, Some("SOMETHING ".to_string()), false).await;
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
            issuer: None,
            algorithms: None,
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

    let (_issuer, key) = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(Algorithm::HS256, key.common.algorithm.unwrap());
    assert_eq!("key2", key.common.key_id.unwrap());
}

#[tokio::test]
async fn it_finds_best_matching_key_with_criteria_algorithm() {
    let jwks_manager = build_jwks_search_components().await;

    let criteria = JWTCriteria {
        kid: None,
        alg: Algorithm::HS256,
    };

    let (_issuer, key) = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(Algorithm::HS256, key.common.algorithm.unwrap());
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

    let (_issuer, key) = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(Algorithm::ES256, key.common.algorithm.unwrap());
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

    let (_issuer, key) = search_jwks(&jwks_manager, &criteria)
        .expect("found a key")
        .pop()
        .expect("list isn't empty");
    assert_eq!(Algorithm::RS256, key.common.algorithm.unwrap());
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
}

fn make_manager(jwk: &Jwk, issuer: Option<String>) -> JwksManager {
    let jwks = JwkSet {
        keys: vec![jwk.clone()],
    };

    let url = Url::from_str("file:///jwks.json").unwrap();
    let list = vec![JwksConfig {
        url: url.clone(),
        issuer,
        algorithms: None,
    }];
    let map = HashMap::from([(url, jwks); 1]);

    JwksManager::new_test(list, map)
}

#[tokio::test]
async fn issuer_check() {
    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_encoded_point(false);

    let encoding_key = EncodingKey::from_ec_der(&signing_key.to_pkcs8_der().unwrap().to_bytes());

    let url_safe_engine = base64::engine::fast_portable::FastPortable::from(
        &base64::alphabet::URL_SAFE,
        base64::engine::fast_portable::NO_PAD,
    );
    let jwk = Jwk {
        common: CommonParameters {
            public_key_use: Some(PublicKeyUse::Signature),
            key_operations: Some(vec![KeyOperations::Verify]),
            algorithm: Some(Algorithm::ES256),
            key_id: Some("hello".to_string()),
            ..Default::default()
        },
        algorithm: AlgorithmParameters::EllipticCurve(EllipticCurveKeyParameters {
            key_type: EllipticCurveKeyType::EC,
            curve: EllipticCurve::P256,
            x: base64::encode_engine(point.x().unwrap(), &url_safe_engine),
            y: base64::encode_engine(point.y().unwrap(), &url_safe_engine),
        }),
    };

    let manager = make_manager(&jwk, Some("hello".to_string()));

    // No issuer
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            iss: None,
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .operation_name("me".to_string())
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&JWTConf::default(), &manager, request.try_into().unwrap()).unwrap() {
        ControlFlow::Break(res) => {
            panic!("unexpected response: {res:?}");
        }
        ControlFlow::Continue(req) => {
            println!("got req with issuer check");
            let claims: Value = req
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
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .operation_name("me".to_string())
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&JWTConf::default(), &manager, request.try_into().unwrap()).unwrap() {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &hyper::body::to_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(response, graphql::Response::builder()
        .errors(vec![graphql::Error::builder().extension_code("AUTH_ERROR").message("Invalid issuer: the token's `iss` was 'hallo', but signed with a key from 'hello'").build()]).build());
        }
        ControlFlow::Continue(req) => {
            println!("got req with issuer check");
            let claims: Value = req
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
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .operation_name("me".to_string())
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&JWTConf::default(), &manager, request.try_into().unwrap()).unwrap() {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &hyper::body::to_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(response, graphql::Response::builder()
            .errors(vec![graphql::Error::builder().extension_code("AUTH_ERROR").message("Invalid issuer: the token's `iss` was 'AAAA', but signed with a key from 'hello'").build()]).build());
        }
        ControlFlow::Continue(_) => {
            panic!("issuer check should have failed")
        }
    }

    // no issuer check
    let manager = make_manager(&jwk, None);
    let token = encode(
        &jsonwebtoken::Header::new(Algorithm::ES256),
        &Claims {
            sub: "test".to_string(),
            exp: get_current_timestamp(),
            iss: Some("hello".to_string()),
        },
        &encoding_key,
    )
    .unwrap();

    let request = supergraph::Request::canned_builder()
        .operation_name("me".to_string())
        .header(http::header::AUTHORIZATION, format!("Bearer {token}"))
        .build()
        .unwrap();

    match authenticate(&JWTConf::default(), &manager, request.try_into().unwrap()).unwrap() {
        ControlFlow::Break(res) => {
            let response: graphql::Response = serde_json::from_slice(
                &hyper::body::to_bytes(res.response.into_body())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(response, graphql::Response::builder()
        .errors(vec![graphql::Error::builder().extension_code("AUTH_ERROR").message("Invalid issuer: the token's `iss` was 'AAAA', but signed with a key from 'hello'").build()]).build());
        }
        ControlFlow::Continue(req) => {
            println!("got req with issuer check");
            let claims: Value = req
                .context
                .get(APOLLO_AUTHENTICATION_JWT_CLAIMS)
                .unwrap()
                .unwrap();
            println!("claims: {claims:?}");
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
            issuer: None,
            algorithms: Some(HashSet::from([Algorithm::RS256])),
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
