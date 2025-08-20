use std::collections::HashMap;
use std::path::PathBuf;

use http::StatusCode;

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
const JWT_WITH_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImVudGl0eV9jYWNoaW5nIiwgImNvcHJvY2Vzc29ycyJdLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc4NzAwMDAwMCwgCiAgImhhbHRBdCI6IDE3ODcwMDAwMDAKfQ.HD_xzVtrXzXp8PdosAircXWPtnVaPRE-N2ZDlv6Llfo"; // gitleaks:allow

const JWT_WITH_COPROCESSORS_SUBSCRIPTION_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWwogICAgImNvcHJvY2Vzc29ycyIsCiAgICAic3Vic2NyaXB0aW9ucyIKICBdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzg3MDAwMDAwLAogICJoYWx0QXQiOiAxNzg3MDAwMDAwCn0.MxjeQOea7wBjvs1J0-44oEfdoaVwKuEexy-JdgZ-3R8"; // gitleaks:allow

const JWT_WITH_ALLOWED_FEATURES_NONE: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc4NzAwMDAwMCwKICAiaGFsdEF0IjogMTc4NzAwMDAwMAp9.LPNJgPY20DH054mXgrzaxEFiME656ZJ-ge5y9Zh3kkc"; // gitleaks:allow

const JWT_WITH_ALLOWED_FEATURES_COPROCESSOR_WITH_FEATURE_UNDEFINED_IN_ROUTER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWwogICAgImNvcHJvY2Vzc29ycyIsCiAgICAicmFuZG9tIiwKICAgICJzdWJzY3JpcHRpb25zIgogIF0sCiAgImlzcyI6ICJodHRwczovL3d3dy5hcG9sbG9ncmFwaHFsLmNvbS8iLAogICJzdWIiOiAiYXBvbGxvIiwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3ODcwMDAwMDAsCiAgImhhbHRBdCI6IDE3ODcwMDAwMDAKfQ.l4O-YLwIu2hjoSq1HseJQMS_9qFNL9v304I7gfLqV3w"; // gitleaks:allow

const JWT_WITH_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImVudGl0eV9jYWNoaW5nIiwgImNvcHJvY2Vzc29ycyIsICJ0cmFmZmljX3NoYXBpbmciXSwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3ODcwMDAwMDAsIAogICJoYWx0QXQiOiAxNzg3MDAwMDAwCn0.HHfLHmDAjTdQwouAJguvWnpxnHsLzTWswQl70gmkMEM"; // gitleaks:allow

const JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_SUBSCRIPTIONS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJlbnRpdHlfY2FjaGluZyIsICJ0cmFmZmljX3NoYXBpbmciLCAic3Vic2NyaXB0aW9ucyJdLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc1NTMwMjQwMCwgCiAgImhhbHRBdCI6IDE3NTUzMDI0MDAKfQ.2TPyUd9BUn3NCc2Kq8WsJS_6V16s2lgitElhf0lNcwg"; // gitleaks:allow

const JWT_PAST_EXPIRY_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJlbnRpdHlfY2FjaGluZyIsICJ0cmFmZmljX3NoYXBpbmciXSwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3NTUzMDI0MDAsIAogICJoYWx0QXQiOiAxNzU1MzAyNDAwCn0.CERblSGfOVmKt6PtfB2LjnY-ahzMsNB4EGajXZfKWU4"; // gitleaks:allow

const JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_ENTITY_CACHING_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImVudGl0eV9jYWNoaW5nIiwgImNvcHJvY2Vzc29ycyIsICJ0cmFmZmljX3NoYXBpbmciXSwKICAiYXVkIjogIlNFTEZfSE9TVEVEIiwgCiAgIndhcm5BdCI6IDE3NjU5MTA0MDAsIAogICJoYWx0QXQiOiAxNzg3MDAwMDAwCn0.33EWawSaU8dv5KqI8QbAzYFa0KKTcvqTXGaJfRkg-DU"; // gitleaks:allow
const JWT_PAST_WARN_AT_BUT_NOT_EXPIRED_WITH_COPROCESSORS_SUBSCRIPTIONS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbInN1YnNjcmlwdGlvbnMiLCAiY29wcm9jZXNzb3JzIl0sCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzU1MzAyNDAwLCAKICAiaGFsdEF0IjogMTc4NzAwMDAwMAp9.nxyKlFquWBijtIOtL8FnknNfAwvBaZh9TFIDcG7NtiE"; // gitleaks:allow

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

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}
