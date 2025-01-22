use std::path::PathBuf;

use tower::BoxError;

use crate::integration::IntegrationTest;

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_incompatible_warnings_on_all() -> Result<(), BoxError> {
    // Ensure that we have the test keys before running
    // Note: The [IntegrationTest] ensures that these test credentials get
    // set before running the router.
    if std::env::var("TEST_APOLLO_KEY").is_err() || std::env::var("TEST_APOLLO_GRAPH_REF").is_err()
    {
        return Ok(());
    };

    let mut router = IntegrationTest::builder()
        .config(
            r#"
            authentication:
              subgraph:
                all:
                  aws_sig_v4:
                    default_chain:
                      profile_name: "my-test-profile"
                      region: "us-east-1"
                      service_name: "lambda"
                      assume_role:
                        role_arn: "test-arn"
                        session_name: "test-session"
                        external_id: "test-id"
        "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .build()
        .await;

    router.start().await;
    router
        .assert_log_contains(r#""subgraphs":"connectors","message":"plugin `authentication` is enabled for connector-enabled subgraphs"#)
        .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
    // Ensure that we have the test keys before running
    // Note: The [IntegrationTest] ensures that these test credentials get
    // set before running the router.
    if std::env::var("TEST_APOLLO_KEY").is_err() || std::env::var("TEST_APOLLO_GRAPH_REF").is_err()
    {
        return Ok(());
    };

    let mut router = IntegrationTest::builder()
        .config(
            r#"
            authentication:
              subgraph:
                subgraphs:
                  connectors:
                    aws_sig_v4:
                      hardcoded:
                        access_key_id: "my-access-key"
                        secret_access_key: "my-secret-access-key"
                        region: "us-east-1"
                        service_name: "vpc-lattice-svcs"
        "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .build()
        .await;

    router.start().await;
    router
        .assert_log_contains(r#""subgraphs":"connectors","message":"plugin `authentication` is enabled for connector-enabled subgraphs"#)
        .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
    // Ensure that we have the test keys before running
    // Note: The [IntegrationTest] ensures that these test credentials get
    // set before running the router.
    if std::env::var("TEST_APOLLO_KEY").is_err() || std::env::var("TEST_APOLLO_GRAPH_REF").is_err()
    {
        return Ok(());
    };

    let mut router = IntegrationTest::builder()
        .config(
            r#"
            authentication:
              subgraph:
                subgraphs:
                  connectors:
                    aws_sig_v4:
                      hardcoded:
                        access_key_id: "my-access-key"
                        secret_access_key: "my-secret-access-key"
                        region: "us-east-1"
                        service_name: "vpc-lattice-svcs"
              connector:
                sources:
                  connectors.something:
                    aws_sig_v4:
                      default_chain:
                        profile_name: "default"
                        region: "us-east-1"
                        service_name: "lambda"
                        assume_role:
                          role_arn: "arn:aws:iam::XXXXXXXXXXXX:role/lambaexecute"
                          session_name: "connector"
        "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .build()
        .await;

    router.start().await;
    router
        .assert_log_contains(r#""subgraph":"connectors","sources":"jsonPlaceholder","message":"plugin `authentication` is enabled for a connector-enabled subgraph"#)
        .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_no_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
    // Ensure that we have the test keys before running
    // Note: The [IntegrationTest] ensures that these test credentials get
    // set before running the router.
    if std::env::var("TEST_APOLLO_KEY").is_err() || std::env::var("TEST_APOLLO_GRAPH_REF").is_err()
    {
        return Ok(());
    };

    let mut router = IntegrationTest::builder()
        .config(
            r#"
            authentication:
              subgraph:
                subgraphs:
                  connectors:
                    aws_sig_v4:
                      hardcoded:
                        access_key_id: "my-access-key"
                        secret_access_key: "my-secret-access-key"
                        region: "us-east-1"
                        service_name: "vpc-lattice-svcs"
              connector:
                sources:
                  connectors.jsonPlaceholder:
                    aws_sig_v4:
                      default_chain:
                        profile_name: "default"
                        region: "us-east-1"
                        service_name: "lambda"
                        assume_role:
                          role_arn: "arn:aws:iam::XXXXXXXXXXXX:role/lambaexecute"
                          session_name: "connector"
        "#,
        )
        .supergraph(PathBuf::from_iter([
            "tests",
            "fixtures",
            "connectors",
            "quickstart.graphql",
        ]))
        .build()
        .await;

    router.start().await;
    router
        .assert_log_not_contains(r#""subgraph":"connectors","sources":"jsonPlaceholder","message":"plugin `authentication` is enabled for a connector-enabled subgraph"#)
        .await;

    Ok(())
}
