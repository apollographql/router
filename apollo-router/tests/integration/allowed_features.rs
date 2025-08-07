// use std::collections::HashMap;
use std::path::PathBuf;

use crate::integration::IntegrationTest;
use crate::integration::subscriptions::CALLBACK_CONFIG;

const LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG: &str =
    "The router is using features not available for your license";
const JWT_WITH_EMPTY_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWyBdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAyMTE1NDYzODYwLCAKICAiaGFsdEF0IjogMjExNTQ2Mzg2MAp9.2hhIlTtejpjjbJa4-IzilBpjbv6FiHRnevaKxBVLBtA"; // gitleaks:allow
const JWT_WITH_COPROCESSOR_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWyJjb3Byb2Nlc3NvciJdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAyMTE1NDYzODYwLCAKICAiaGFsdEF0IjogMjExNTQ2Mzg2MAp9.64CkaHH6_zd_pl4sP1xHceDe6NbUSbTbR6GcjcRmDc0"; // gitleaks:allow
const JWT_WITH_CONNECTORS_ENTITY_CACHING_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWyJyZXN0X2Nvbm5lY3RvciIsICJlbnRpdHlfY2FjaGluZyJdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAyMTE1NDYzODYwLCAKICAiaGFsdEF0IjogMjExNTQ2Mzg2MAp9.VcmOipKuJW1HvBhlMOdtWgdccIQa5i1ziI40NIxdcO0"; // gitleaks:allow
const JWT_WITH_COPROCESSOR_SUBSCRIPTION_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWyJjb3Byb2Nlc3NvciIsICJzdWJzY3JpcHRpb24iXSwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMjExNTQ2Mzg2MCwgCiAgImhhbHRBdCI6IDIxMTU0NjM4NjAKfQ.HuSTXbA8wGRuLCgaB9EhO1hRnLcdiAY419gIPtFRi84"; // gitleaks:allow
const JWT_WITH_COPROCESSOR_SUBSCRIPTION_DEMAND_CONTROL_COST_IN_ALLOWED_FEATURES: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiYWxsb3dlZEZlYXR1cmVzIjogWyJjb3Byb2Nlc3NvciIsICJzdWJzY3JpcHRpb24iLCAiZGVtYW5kX2NvbnRyb2xfY29zdCJdLAogICJpc3MiOiAiaHR0cHM6Ly93d3cuYXBvbGxvZ3JhcGhxbC5jb20vIiwKICAic3ViIjogImFwb2xsbyIsCiAgImF1ZCI6ICJTRUxGX0hPU1RFRCIsIAogICJ3YXJuQXQiOiAyMTE1NDYzODYwLCAKICAiaGFsdEF0IjogMjExNTQ2Mzg2MAp9.TwQC2cEwCDil_KGLr7pq4e7Ts9my97TaFvKDYtyzBH8"; // gitleaks:allow
const JWT_WITH_ALLOWED_FEATURES_NONE: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMjExNTQ2Mzg2MCwgCiAgImhhbHRBdCI6IDIxMTU0NjM4NjAKfQ.QNr-ZzE6xKMjiTS4wDNd05DcGLhZXvYxJmy2gScGJN8"; // gitleaks:allow
const JWT_WITH_ALLOWED_FEATURES_COPROCESSOR_WITH_FEATURE_UNDEFINED_IN_ROUTER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.ewogICJleHAiOiAxMDAwMDAwMDAwMCwKICAiaXNzIjogImh0dHBzOi8vd3d3LmFwb2xsb2dyYXBocWwuY29tLyIsCiAgImFsbG93ZWRGZWF0dXJlcyI6IFsiY29wcm9jZXNzb3IiLCAicmFuZG9tIl0sCiAgInN1YiI6ICJhcG9sbG8iLAogICJhdWQiOiAiU0VMRl9IT1NURUQiLCAKICAid2FybkF0IjogMjExNTQ2Mzg2MCwgCiAgImhhbHRBdCI6IDIxMTU0NjM4NjAKfQ.qNpK1w24jPyMtzkclIW8uaYNIsczznHF38-L8xPyNuQ"; // gitleaks:allow

#[tokio::test(flavor = "multi_thread")]
async fn connectors_with_entity_caching_enabled_when_allowed_features_contains_both_features() {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            preview_entity_cache:
                enabled: true
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
        .jwt(JWT_WITH_CONNECTORS_ENTITY_CACHING_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_enabled_when_allowed_features_contains_subscription() {
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .jwt(JWT_WITH_COPROCESSOR_SUBSCRIPTION_DEMAND_CONTROL_COST_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

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

// NB: this behavior will change once allowed_features claim is contained in all licenses
#[tokio::test(flavor = "multi_thread")]
async fn subscription_enabled_when_allowed_features_none() {
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .jwt(JWT_WITH_ALLOWED_FEATURES_NONE.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn coprocessor_enabled_when_allowed_features_contains_coprocessor_and_other_features() {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .jwt(JWT_WITH_COPROCESSOR_SUBSCRIPTION_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn coprocessor_demand_control_enabled_when_allowed_features_contains_features() {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .jwt(JWT_WITH_COPROCESSOR_SUBSCRIPTION_DEMAND_CONTROL_COST_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router.assert_started().await;
    router.assert_no_error_logs();
}

#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_allowed_features_does_not_contain_feature_demand_control() {
    let mock_server = wiremock::MockServer::start().await;
    let coprocessor_address = mock_server.uri();

    let mut router = IntegrationTest::builder()
        .config(
            include_str!("fixtures/coprocessor_demand_control.router.yaml")
                .replace("<replace>", &coprocessor_address),
        )
        .jwt(JWT_WITH_COPROCESSOR_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

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
async fn feature_violation_when_allowed_emopty_with_subscripton_in_config() {
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .jwt(JWT_WITH_EMPTY_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn feature_violation_when_allowed_does_not_contain_feature_with_subscripton_in_config() {
    let mut router = IntegrationTest::builder()
        .supergraph("tests/integration/subscriptions/fixtures/supergraph.graphql")
        .config(CALLBACK_CONFIG)
        .jwt(JWT_WITH_COPROCESSOR_IN_ALLOWED_FEATURES.to_string())
        .build()
        .await;

    router.start().await;
    router
        .assert_error_log_contained(LICENSE_ALLOWED_FEATURES_DOES_NOT_INCLUDE_FEATURE_MSG)
        .await;
}

// TODO-Ellie:
// Add test for when we get an unexpcted feature that gets mapped to other
