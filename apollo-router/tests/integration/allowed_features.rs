use std::collections::HashMap;
use std::path::PathBuf;

use crate::integration::IntegrationTest;
use crate::integration::common::TEST_JWKS_ENDPOINT;

// NOTE: if these tests fail for haltAt/warnAt related reasons (that they're in the past), go to
// jwt.io and doublecheck that those claims are still sensible. There's an issue when using
// Instants to schedule things (like we do for license streams) if those Instants are derived from
// some far-future SystemTime: tokio has an upper bound for how far out it schedules, putting a
// pretty hard limit (about a year) for what we can set the haltAt/warnAt values in JWTs to

const LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG: &str =
    "license violation, the router is using features not available for your license";
const LICENSE_EXPIRED_MESSAGE: &str =
    "License has expired. The Router will no longer serve requests.";

const JWT_WITH_EMPTY_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogW10sCiAgImlzcyI6ICJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLAogICJzdWIiOiAiYXBvbGxvIiwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3ODcwMDAwMDAsCiAgImhhbHRBdCI6IDE3ODcwMDAwMDAKfQ.nERzNxBzt7KLgBD4ouHydbht6_1jgyCYF8aKzFKGjhI"; // gitleaks:allow

const JWT_WITH_COPROCESSORS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWyJjb3Byb2Nlc3NvcnMiXSwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc4NzAwMDAwMCwKICAiaGFsdEF0IjogMTc4NzAwMDAwMAp9.UD2JZtyvCSY6oXeDOsmWZehNGQjDqdhOiw-1f2TW4Og"; // gitleaks:allow

// In the CI environment we only install Redis on x86_64 Linux; this jwt is part of testing that
// flow
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
const JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWwogICAgImNvcHJvY2Vzc29ycyIsCiAgICAiZW50aXR5X2NhY2hpbmciLAogICAgImNvbm5lY3RvcnMiCiAgXSwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc4NzAwMDAwMCwKICAiaGFsdEF0IjogMTc4NzAwMDAwMAp9.YusQdchif3OfqbSuiNUf6PjBVjaDsagro-0Ihm8L0BI"; // gitleaks:allow

const JWT_WITH_COPROCESSORS_SUBSCRIPTION_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWwogICAgImNvcHJvY2Vzc29ycyIsCiAgICAic3Vic2NyaXB0aW9ucyIKICBdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzg3MDAwMDAwLAogICJoYWx0QXQiOiAxNzg3MDAwMDAwCn0.MxjeQOea7wBjvs1J0-44oEfdoaVwKuEexy-JdgZ-3R8"; // gitleaks:allow

const JWT_WITH_ALLOWED_FEATURES_NONE: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc4NzAwMDAwMCwKICAiaGFsdEF0IjogMTc4NzAwMDAwMAp9.LPNJgPY20DH054mXgrzaxEFiME656ZJ-ge5y9Zh3kkc"; // gitleaks:allow

const JWT_WITH_ALLOWED_FEATURES_COPROCESSOR_WITH_FEATURE_UNDEFINED_IN_ROUTER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWwogICAgImNvcHJvY2Vzc29ycyIsCiAgICAicmFuZG9tIiwKICAgICJzdWJzY3JpcHRpb25zIgogIF0sCiAgImlzcyI6ICJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLAogICJzdWIiOiAiYXBvbGxvIiwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3ODcwMDAwMDAsCiAgImhhbHRBdCI6IDE3ODcwMDAwMDAKfQ.l4O-YLwIu2hjoSq1HseJQMS_9qFNL9v304I7gfLqV3w"; // gitleaks:allow

const JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWwogICAgImNvcHJvY2Vzc29ycyIsCiAgICAiZW50aXR5X2NhY2hpbmciLAogICAgInRyYWZmaWNfc2hhcGluZyIsCiAgICAiY29ubmVjdG9ycyIKICBdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzg3MDAwMDAwLAogICJoYWx0QXQiOiAxNzg3MDAwMDAwCn0.jr0BY6eoecQhHWg7toOdvXzDZTrZI6gaPDA4TS98MQA"; // gitleaks:allow

const JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_SUBSCRIPTIONS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJlbnRpdHlfY2FjaGluZyIsICJ0cmFmZmljX3NoYXBpbmciLCAic3Vic2NyaXB0aW9ucyJdLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc1NTMwMjQwMCwgCiAgImhhbHRBdCI6IDE3NTUzMDI0MDAKfQ.2TPyUd9BUn3NCc2Kq8WsJS_6V16s2lgitElhf0lNcwg"; // gitleaks:allow

const JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJlbnRpdHlfY2FjaGluZyIsICJ0cmFmZmljX3NoYXBpbmciXSwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3NTUzMDI0MDAsIAogICJoYWx0QXQiOiAxNzU1MzAyNDAwCn0.CERblSGfOVmKt6PtfB2LjnY-ahzMsNB4EGajXZfKWU4"; // gitleaks:allow

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
        .jwt(
            JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

// In the CI environment we only install Redis on x86_64 Linux
#[cfg(any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux")))]
#[tokio::test(flavor = "multi_thread")]
async fn connectors_with_entity_caching_enabled_when_allowed_features_contains_both_features() {
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
        .jwt(JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES.to_string())
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

#[tokio::test(flavor = "multi_thread")]
async fn oss_feature_enabled_when_allowed_features_empty() {
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
async fn license_violation_when_allowed_features_does_not_contain_file_uploads() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );
    let mut router = IntegrationTest::builder()
        .config(FILE_UPLOADS_CONFIG)
        .env(env)
        .jwt(
            JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

/*
 * GIVEN
 *  - an expired license
 *  - a valid config
 *  - a valid schema
 *
 * THEN
 *  - since the license is expired and using restricted features the router should not start
 * */
#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_license_expired_allowed_features_contains_feature() {
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

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
    router
        .assert_error_log_contained(LICENSE_EXPIRED_MESSAGE)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_license_expired_allowed_features_does_not_contain_feature() {
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
            JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;

    router.replace_config_string("http://localhost:{{PRODUCTS_PORT}}", "localhost:4001");
    router.replace_config_string("http://localhost:{{ACCOUNTS_PORT}}", "localhost:4002");

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
    router
        .assert_error_log_contained(LICENSE_EXPIRED_MESSAGE)
        .await;
}

/*
 * GIVEN
 *  - an expired license
 *  - a valid config that does not use any restricted features
 *  - a valid schema
 *
 * THEN
 *  - since we are not using any restricted features the router should start
 * */
#[tokio::test(flavor = "multi_thread")]
async fn router_starts_with_expired_license_when_not_using_any_restricted_features() {
    let mut env = HashMap::new();
    env.insert(
        "APOLLO_TEST_INTERNAL_UPLINK_JWKS".to_string(),
        TEST_JWKS_ENDPOINT.as_os_str().into(),
    );

    // Connectors and APQ are available to oss+
    let mut router = IntegrationTest::builder()
        .config(
            r#"
                apq:
                  subgraph:
                    all:
                      enabled: false
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
        .jwt(
            JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES
                .to_string(),
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
}
