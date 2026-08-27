use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use http::StatusCode;

use crate::integration::IntegrationTest;
use crate::integration::common::LICENSE_SIX_MONTHS_SECS;
use crate::integration::common::TEST_JWKS_ENDPOINT;
use crate::integration::common::mint_license_jwt;

const LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG: &str =
    "license violation, the router is using features not available for your license";
const LICENSE_EXPIRED_MESSAGE: &str =
    "License has expired. The Router will no longer serve requests.";

// The license JWTs below are minted at runtime with `warnAt`/`haltAt` relative
// to now, so they can never silently expire the way pinned tokens used to.
// `LICENSE_SIX_MONTHS_SECS` keeps future claims within tokio's DelayQueue
// scheduler cap (about a year) — see `mint_license_jwt` in common.rs.

static JWT_WITH_EMPTY_ALLOWED_FEATURES: LazyLock<String> =
    LazyLock::new(|| mint_license_jwt(Some(&[]), LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS));

static JWT_WITH_COPROCESSORS_IN_ALLOWED_FEATURES: LazyLock<String> = LazyLock::new(|| {
    mint_license_jwt(
        Some(&["coprocessors"]),
        LICENSE_SIX_MONTHS_SECS,
        LICENSE_SIX_MONTHS_SECS,
    )
});

// In the CI environment we only install Redis on x86_64 Linux; this jwt is part of testing that
// flow
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
static JWT_WITH_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES: LazyLock<String> =
    LazyLock::new(|| {
        mint_license_jwt(
            Some(&["entity_caching", "coprocessors"]),
            LICENSE_SIX_MONTHS_SECS,
            LICENSE_SIX_MONTHS_SECS,
        )
    });

static JWT_WITH_COPROCESSORS_SUBSCRIPTION_IN_ALLOWED_FEATURES: LazyLock<String> =
    LazyLock::new(|| {
        mint_license_jwt(
            Some(&["coprocessors", "subscriptions"]),
            LICENSE_SIX_MONTHS_SECS,
            LICENSE_SIX_MONTHS_SECS,
        )
    });

static JWT_WITH_ALLOWED_FEATURES_NONE: LazyLock<String> =
    LazyLock::new(|| mint_license_jwt(None, LICENSE_SIX_MONTHS_SECS, LICENSE_SIX_MONTHS_SECS));

static JWT_WITH_ALLOWED_FEATURES_COPROCESSOR_WITH_FEATURE_UNDEFINED_IN_ROUTER: LazyLock<String> =
    LazyLock::new(|| {
        mint_license_jwt(
            Some(&["coprocessors", "random", "subscriptions"]),
            LICENSE_SIX_MONTHS_SECS,
            LICENSE_SIX_MONTHS_SECS,
        )
    });

static JWT_WITH_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: LazyLock<String> =
    LazyLock::new(|| {
        mint_license_jwt(
            Some(&["entity_caching", "coprocessors", "traffic_shaping"]),
            LICENSE_SIX_MONTHS_SECS,
            LICENSE_SIX_MONTHS_SECS,
        )
    });

static JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_SUBSCRIPTIONS_IN_ALLOWED_FEATURES:
    LazyLock<String> = LazyLock::new(|| {
    mint_license_jwt(
        Some(&[
            "coprocessors",
            "entity_caching",
            "traffic_shaping",
            "subscriptions",
        ]),
        -LICENSE_SIX_MONTHS_SECS,
        -LICENSE_SIX_MONTHS_SECS,
    )
});

static JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES:
    LazyLock<String> = LazyLock::new(|| {
    mint_license_jwt(
        Some(&["coprocessors", "entity_caching", "traffic_shaping"]),
        -LICENSE_SIX_MONTHS_SECS,
        -LICENSE_SIX_MONTHS_SECS,
    )
});

static JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES:
    LazyLock<String> = LazyLock::new(|| {
    mint_license_jwt(
        Some(&["entity_caching", "coprocessors", "traffic_shaping"]),
        -LICENSE_SIX_MONTHS_SECS,
        LICENSE_SIX_MONTHS_SECS,
    )
});

static JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_SUBSCRIPTIONS_IN_ALLOWED_FEATURES:
    LazyLock<String> = LazyLock::new(|| {
    mint_license_jwt(
        Some(&["subscriptions", "coprocessors"]),
        -LICENSE_SIX_MONTHS_SECS,
        LICENSE_SIX_MONTHS_SECS,
    )
});

const SUBSCRIPTION_CONFIG: &str = include_str!("subscriptions/fixtures/subscription.router.yaml");
const SUBSCRIPTION_COPROCESSOR_CONFIG: &str =
    include_str!("subscriptions/fixtures/subscription_coprocessor.router.yaml");
const FILE_UPLOADS_CONFIG: &str =
    include_str!("../../tests/fixtures/file_upload/default.router.yaml");

/*
 * GIVEN
 *  - a valid license whose `allowed_features` claim contains the feature
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - since the feature is part of the `allowed_features` set
 *    the router should start successfully with no license violations
 * */
#[tokio::test(flavor = "multi_thread")]
async fn traffic_shaping_when_allowed_features_contains_feature() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );

    let mut router = IntegrationTest::builder()
        .config(
            r#"
            telemetry:
                exporters:
                    metrics:
                        prometheus:
                            listen: 127.0.0.1:4000
                            enabled: true
                            path: /metrics
            include_subgraph_errors:
                all: true
            traffic_shaping:
                all:
                    timeout: 1ns
            "#,
        )
        .env(env)
        .jwt(JWT_WITH_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
#[tokio::test(flavor = "multi_thread")]
async fn connectors_with_entity_caching_enabled_when_allowed_features_contains_features() {
    use crate::integration::common::TEST_JWKS_ENDPOINT;

    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            preview_entity_cache:
              enabled: true
              subgraph:
                all:
                  redis:
                    urls: ["redis://127.0.0.1:6379"]
                    ttl: "10m"
                    required_to_start: true
                subgraphs:
                    connectors:
                      enabled: true
    "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .env(env)
        .jwt(JWT_WITH_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_coprocessors_enabled_when_allowed_features_contains_both_features() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph(PathBuf::from_iter([
            "tests",
            "integration",
            "subscriptions",
            "fixtures",
            "supergraph.graphql",
        ]))
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        .jwt(JWT_WITH_COPROCESSORS_SUBSCRIPTION_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "5000");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "5001");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

/*
 * GIVEN
 *  - a valid license that does not contain an `allowed_features` claim
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - router should start successfully
 *  NB: this behavior will change once allowed_features claim is contained in all licenses
*/
#[tokio::test(flavor = "multi_thread")]
async fn oss_feature_apq_enabled_when_allowed_features_empty() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            apq:
              enabled: true
    "#,
        )
        .env(env)
        .jwt(JWT_WITH_EMPTY_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    // Apq is an oss feature
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn oss_feature_file_uploads_enabled_with_non_empty_allowed_features() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .config(FILE_UPLOADS_CONFIG)
        .env(env)
        .jwt(JWT_WITH_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    // File uploads is an oss plugin
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn router_starts_when_allowed_features_contains_feature_undefined_in_router() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .env(env)
        .jwt(JWT_WITH_ALLOWED_FEATURES_COPROCESSOR_WITH_FEATURE_UNDEFINED_IN_ROUTER.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

/*
 * GIVEN
 *  - a valid license that does not contain an `allowed_features` claim
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - router should start successfully
 *  NB: this behavior will change once allowed_features claim is contained in all licenses
*/
#[tokio::test(flavor = "multi_thread")]
async fn subscription_coprocessors_enabled_when_allowed_features_none() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph(PathBuf::from_iter([
            "tests",
            "integration",
            "subscriptions",
            "fixtures",
            "supergraph.graphql",
        ]))
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        .jwt(JWT_WITH_ALLOWED_FEATURES_NONE.to_string())
        .build()
        .await;
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "5000");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "5001");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn demand_control_enabledwhen_allowed_features_none() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .env(env)
        .jwt(JWT_WITH_ALLOWED_FEATURES_NONE.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

/*
 * GIVEN
 *  - a valid license whose `allowed_features` claim is empty (does not contain any features)
 *    or more features
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - since the feature(s) is/are not part of the `allowed_features` set
 *    the router should should emit an error log containing the license violations
 * */
#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_allowed_features_empty_with_coprocessor_in_config() {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .env(env)
        .jwt(JWT_WITH_EMPTY_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_allowed_features_empty_with_subscripton_in_config() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_CONFIG)
        .env(env)
        .jwt(JWT_WITH_EMPTY_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

/*
 * GIVEN
 *  - a valid license whose `allowed_features` claim does not contain one
 *    or more features
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - since the feature(s) is/are not part of the `allowed_features` set
 *    the router should should emit an error log containing the license violations
 * */
#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_allowed_features_does_not_contain_feature_demand_control() {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .env(env)
        .jwt(JWT_WITH_COPROCESSORS_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_allowed_features_with_coprocessor_only_with_subscripton_and_coprocessor_in_config()
 {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        .jwt(JWT_WITH_COPROCESSORS_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn canned_response_when_license_halted_with_valid_config_and_schema() {
    /*
     * GIVEN
     *  - an expired license
     *  - a valid config
     *  - a valid schema
     * */

    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        .jwt(JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_SUBSCRIPTIONS_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "localhost:4001");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "localhost:4002");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    /*
     * THEN
     *  - since the license is expired and using restricted features the router should start but
     *    the axum middleware, license_handler, should return a 500
     * */
    router.start().await;
    router
        .assert_error_log_contained(LICENSE_EXPIRED_MESSAGE)
        .await;

    let (_, response) = router.execute_default_query().await;
    // We expect the axum middleware for handling halted licenses to return a server error
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test(flavor = "multi_thread")]
async fn canned_response_when_license_halted_with_restricted_config_and_valid_schema() {
    /*
     * GIVEN
     *  - an expired license
     *  - an invalid config - that contains a feature not in the allowedFeatures claim
     *  - a valid schema
     * */

    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    // subscriptions not an allowed feature--config invalid
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        // jwt's allowed features does not contain subscriptions
        .jwt(
            JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;

    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "localhost:4001");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "localhost:4002");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    /*
     * THEN
     *  - since the license is expired and using restricted features the router should start but
     *    the axum middleware, license_handler, should return a 500
     * */
    router.start().await;
    router
        .assert_error_log_contained(LICENSE_EXPIRED_MESSAGE)
        .await;

    let (_, response) = router.execute_default_query().await;
    // We expect the axum middleware for handling halted licenses to return a server error
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test(flavor = "multi_thread")]
async fn canned_response_when_license_halted_with_valid_config_and_invalid_schema() {
    /*
     * GIVEN
     *  - an expired license
     *  - a valid config
     *  - a invalid schema - that contains a feature not in the allowedFeatures claim
     * */

    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );

    // contextArgument is restricted for this JWT
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/fixtures/authenticated_directive.graphql")
        .config(FILE_UPLOADS_CONFIG)
        .env(env)
        .jwt(JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_SUBSCRIPTIONS_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "localhost:4001");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "localhost:4002");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    /*
     * THEN
     *  - since the license is expired and using restricted features the router should start but
     *    the axum middleware, license_handler, should return a 500
     * */
    router.start().await;
    router
        .assert_error_log_contained(LICENSE_EXPIRED_MESSAGE)
        .await;

    let (_, response) = router.execute_default_query().await;
    // We expect the axum middleware for handling halted licenses to return a server error
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/*
 * GIVEN
 *  - a license past the warnAt date but not yet expired but the features in use contained
 *    in the allowedFeatures claim
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - since the license is not yet expired, the router should start with restricted features in use
 * */
#[tokio::test(flavor = "multi_thread")]
async fn router_starts_when_license_past_warn_at_but_not_expired_allowed_features_contains_feature_subscriptions()
 {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        .jwt(
            JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_SUBSCRIPTIONS_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;

    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "localhost:4001");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "localhost:4002");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    router.start().await;
    router.assert_started().await;
}

// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
#[tokio::test(flavor = "multi_thread")]
async fn router_starts_when_license_past_warn_at_but_not_expired_allowed_features_contains_feature_entity_caching()
 {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            preview_entity_cache:
              enabled: true
              subgraph:
                all:
                  redis:
                    urls: ["redis://127.0.0.1:6379"]
                    ttl: "10m"
                    required_to_start: true
                subgraphs:
                    connectors:
                      enabled: true
    "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .env(env)
        .jwt(JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_license_past_warn_at_but_not_expired_allowed_features_does_not_contain_feature()
 {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
        .env(env)
        // jwt's allowed features does not contain subscriptions
        .jwt(
            JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;
    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "5000");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "5001");
    router.replace_config_string("http://localhost:{{COPROCESSOR_PORT}}", "5002");

    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "localhost:4001");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "localhost:4002");

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}
