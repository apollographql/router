use std::path::PathBuf;

use crate::integration::IntegrationTest;

const LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG: &str =
    "The router is using features not available for your license";
const JWT_WITH_EMPTY_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbIF0sCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzYyODE5MjAwLCAKICAiaGFsdEF0IjogMTc2MjgxOTIwMAp9.sQ_921kFUtmTnMc9NwwaK7aG9k-H9mHvuwH2F0FNKYM"; // gitleaks:allow
const JWT_WITH_COPROCESSORS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyJdLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc2MjgxOTIwMCwgCiAgImhhbHRBdCI6IDE3NjI4MTkyMDAKfQ.jxNKaugok1pbme-JrrYhA48GJN9rJ72dtbf8mUVIvIo"; // gitleaks:allow
const JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJlbnRpdHlfY2FjaGluZyIsICJjb25uZWN0b3JzIl0sCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzYyODE5MjAwLCAKICAiaGFsdEF0IjogMTc2MjgxOTIwMAp9.YLQefPKtiw6-RFhImhnS4fRhBMG65TnWt2HILUqIpUI"; // gitleaks:allow
const JWT_WITH_COPROCESSORS_SUBSCRIPTION_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJzdWJzY3JpcHRpb25zIl0sCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAxNzYyODE5MjAwLCAKICAiaGFsdEF0IjogMTc2MjgxOTIwMAp9.yTLX4qlt8vSowmEpsDCbmcyqOc-sV9ps5tm_ZcuvbRg"; // gitleaks:allow
const JWT_WITH_ALLOWED_FEATURES_NONE: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc2MjgxOTIwMCwgCiAgImhhbHRBdCI6IDE3NjI4MTkyMDAKfQ.h9K7ag7Ybr6K0mhe1MSeGMD2eLl4PRPLJSgbA3oGXGc"; // gitleaks:allow
const JWT_WITH_ALLOWED_FEATURES_COPROCESSOR_WITH_FEATURE_UNDEFINED_IN_ROUTER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJyYW5kb20iLCAic3Vic2NyaXB0aW9ucyJdLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc2MjgxOTIwMCwgCiAgImhhbHRBdCI6IDE3NjI4MTkyMDAKfQ.wSe11pY09ymL2SUkgYTh8lObHL1c2txB5s9r_yvr_-U"; // gitleaks:allow
const JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_TRAFFIC_SHAPING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhbGxvd2VkRmVhdHVyZXMiOiBbImNvcHJvY2Vzc29ycyIsICJlbnRpdHlfY2FjaGluZyIsICJ0cmFmZmljX3NoYXBpbmciLCAiY29ubmVjdG9ycyJdLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMTc2MjgxOTIwMCwgCiAgImhhbHRBdCI6IDE3NjI4MTkyMDAKfQ.4Fq5mkipJzXVqwEcgSq-rEcZ_-ShsmR02Z6kQDYiNak"; // gitleaks:allow

const SUBSCRIPTION_CONFIG: &str = include_str!("subscriptions/fixtures/subscription.router.yaml");
pub const SUBSCRIPTION_COPROCESSOR_CONFIG: &str =
    include_str!("subscriptions/fixtures/subscription_coprocessor.router.yaml");

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

#[tokio::test(flavor = "multi_thread")]
async fn connectors_with_entity_caching_enabled_when_allowed_features_contains_both_features() {
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
        .jwt(JWT_WITH_CONNECTORS_ENTITY_CACHING_COPROCESSORS_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_coprocessors_enabled_when_allowed_features_contains_both_features() {
    let mut router = IntegrationTest::builder()
        .supergraph(PathBuf::from_iter([
            "tests",
            "integration",
            "subscriptions",
            "fixtures",
            "supergraph.graphql",
        ]))
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
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
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            apq:
              enabled: true
    "#,
        )
        .jwt(JWT_WITH_EMPTY_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn router_starts_when_allowed_features_contains_feature_undefined_in_router() {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
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
    let mut router = IntegrationTest::builder()
        .supergraph(PathBuf::from_iter([
            "tests",
            "integration",
            "subscriptions",
            "fixtures",
            "supergraph.graphql",
        ]))
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
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
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
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

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
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
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_CONFIG)
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

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
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
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(SUBSCRIPTION_COPROCESSOR_CONFIG)
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
    let mut router = IntegrationTest::builder()
        .config(include_str!(
            "../../tests/fixtures/file_upload/default.router.yaml"
        ))
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
