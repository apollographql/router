mod apq {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                apq:
                  subgraph:
                    all:
                      enabled: true
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `apq` indirectly targets a connector-enabled subgraph, which is not supported"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

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
            .build()
            .await;

        router.start().await;
        router
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `apq` is explicitly configured for connector-enabled subgraph"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                apq:
                  subgraph:
                    all:
                      enabled: true
                    subgraphs:
                      connectors:
                        enabled: false
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
            .assert_log_not_contains(r#""subgraph":"connectors","message":"plugin `apq`"#)
            .await;

        Ok(())
    }
}

mod authentication {
    use std::path::PathBuf;

    use serde_json::Value;
    use serde_json::json;
    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::Query;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
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
            .wait_for_log_message(r#""subgraphs":"connectors","message":"plugin `authentication` is enabled for connector-enabled subgraphs"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
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
            .wait_for_log_message(r#""subgraphs":"connectors","message":"plugin `authentication` is enabled for connector-enabled subgraphs"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_with_overrides() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
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
            .wait_for_log_message(r#""subgraph":"connectors","sources":"jsonPlaceholder","message":"plugin `authentication` is enabled for a connector-enabled subgraph"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
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

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(not(feature = "ci"), ignore)]
    async fn test_aws_sig_v4_signing() {
        let mut router = IntegrationTest::builder()
            .config(include_str!("fixtures/connectors_sigv4.router.yaml"))
            .supergraph(PathBuf::from(
                "tests/integration/fixtures/connectors_sigv4.graphql",
            ))
            .build()
            .await;

        router.start().await;
        router.assert_started().await;

        let (_, response) = router
            .execute_query(
                Query::builder()
                    .body(json! ({"query": "query { instances }"}))
                    .build(),
            )
            .await;
        let body: Value = response.json().await.unwrap();
        router.graceful_shutdown().await;
        let body = body.as_object().expect("Response body should be object");
        let errors = body.get("errors");
        assert!(errors.is_none(), "query generated errors: {errors:?}");
        let me = body
            .get("data")
            .expect("Response body should have data")
            .as_object()
            .expect("Data should be object")
            .get("instances")
            .expect("Data should have instances");
        assert!(me.is_null());
    }
}

mod batching {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
            batching:
              enabled: true
              mode: batch_http_link
              subgraph:
                all:
                  enabled: true
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `batching` indirectly targets a connector-enabled subgraph, which is not supported"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
            batching:
              enabled: true
              mode: batch_http_link
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
            .build()
            .await;

        router.start().await;
        router
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `batching` is explicitly configured for connector-enabled subgraph"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
            batching:
              enabled: true
              mode: batch_http_link
              subgraph:
                all:
                  enabled: true
                subgraphs:
                  connectors:
                    enabled: false
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
            .assert_log_not_contains(r#""subgraph":"connectors","message":"plugin `batching`"#)
            .await;

        Ok(())
    }
}

mod coprocessor {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                coprocessor:
                  url: http://127.0.0.1:8081
                  subgraph:
                    all:
                      request: {}
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
            .wait_for_log_message(r#""subgraphs":"connectors","message":"coprocessors which hook into `subgraph_request` or `subgraph_response`"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_for_supergraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                coprocessor:
                  url: http://127.0.0.1:8081
                  supergraph:
                      request: {}
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
            .assert_log_not_contains(r#""subgraphs":"connectors","message":"coprocessors which hook into `subgraph_request` or `subgraph_response`"#)
            .await;

        Ok(())
    }
}

mod entity_cache {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                preview_entity_cache:
                  enabled: true
                  subgraph:
                    all:
                      enabled: true
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `preview_entity_cache` indirectly targets a connector-enabled subgraph, which is not supported"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

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
            .build()
            .await;

        router.start().await;
        router
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `preview_entity_cache` is explicitly configured for connector-enabled subgraph"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

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
                        enabled: false
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
            .assert_log_not_contains(
                r#""subgraph":"connectors","message":"plugin `preview_entity_cache`"#,
            )
            .await;

        Ok(())
    }
}

mod headers {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
            headers:
              all:
                request:
                  - propagate:
                      matching: ^upstream-header-.*
                  - remove:
                      named: "x-legacy-account-id"
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `headers` indirectly targets a connector-enabled subgraph"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
            headers:
              subgraphs:
                connectors:
                  request:
                    - propagate:
                        matching: ^upstream-header-.*
                    - remove:
                        named: "x-legacy-account-id"
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `headers` is explicitly configured for connector-enabled subgraph"#)
            .await;

        Ok(())
    }
}

mod rhai {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                rhai:
                  main: "test.rhai"
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
            .wait_for_log_message(r#""subgraphs":"connectors","message":"rhai scripts which hook into `subgraph_request` or `subgraph_response`"#)
            .await;

        Ok(())
    }
}

mod telemetry {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_all() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
            telemetry:
              apollo:
                errors:
                  subgraph:
                    all:
                      send: true
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `telemetry` is indirectly configured to send errors to Apollo studio for a connector-enabled subgraph, which is only supported when `preview_extended_error_metrics` is enabled"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                telemetry:
                  apollo:
                    errors:
                      subgraph:
                        all:
                          send: false
                        subgraphs:
                          connectors:
                            send: true
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"plugin `telemetry` is explicitly configured to send errors to Apollo studio for connector-enabled subgraph, which is only supported when `preview_extended_error_metrics` is enabled"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_with_overrides() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                telemetry:
                  apollo:
                    errors:
                      subgraph:
                        all:
                          send: true
                        subgraphs:
                          connectors:
                            send: false
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
            .assert_log_not_contains(r#""subgraph":"connectors","message":"plugin `telemetry`"#)
            .await;

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_incompatible_warnings_with_flag_enabled() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                telemetry:
                  apollo:
                    errors:
                      preview_extended_error_metrics: enabled
                      subgraph:
                        all:
                          send: true
                        subgraphs:
                          connectors:
                            send: true
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
            .assert_log_not_contains(r#""subgraph":"connectors","message":"plugin `telemetry`"#)
            .await;

        Ok(())
    }
}

mod tls {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                tls:
                  subgraph:
                    subgraphs:
                      connectors:
                        certificate_authorities: "${file./path/to/product_ca.crt}"
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"The `tls` plugin is explicitly configured for a subgraph containing connectors, which is not supported. Instead, configure the connector sources directly using `tls.connector.sources.<subgraph_name>.<source_name>`."#)
            .await;

        Ok(())
    }
}

mod traffic_shaping {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                traffic_shaping:
                  subgraphs:
                    connectors:
                      deduplicate_query: true
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"The `traffic_shaping` plugin is explicitly configured for a subgraph containing connectors, which is not supported. Instead, configure the connector sources directly using `traffic_shaping.connector.sources.<subgraph_name>.<source_name>`."#)
            .await;

        Ok(())
    }
}

mod url_override {
    use std::path::PathBuf;

    use tower::BoxError;

    use crate::integration::IntegrationTest;
    use crate::integration::common::graph_os_enabled;

    #[tokio::test(flavor = "multi_thread")]
    async fn incompatible_warnings_on_subgraph() -> Result<(), BoxError> {
        // Ensure that we have the test keys before running
        // Note: The [IntegrationTest] ensures that these test credentials get
        // set before running the router.
        if !graph_os_enabled() {
            return Ok(());
        };

        let mut router = IntegrationTest::builder()
            .config(
                r#"
                override_subgraph_url:
                  connectors: http://localhost:8080
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
            .wait_for_log_message(r#""subgraph":"connectors","message":"overriding a subgraph URL for a connectors-enabled subgraph is not supported"#)
            .await;

        Ok(())
    }
}
