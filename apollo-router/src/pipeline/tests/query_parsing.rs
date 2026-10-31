//! Tests for the query parsing service pipeline.

use tower::Service;
use tower::ServiceExt;

use super::test_configuration;
use super::test_schema;
use crate::compute_job::MaybeBackPressureError;
use crate::metrics::FutureMetricsExt;
use crate::pipeline::stages::build_query_parsing_service;
use crate::services::query_parsing::Request;

#[tokio::test]
async fn same_query_is_cache_hit() {
    async {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let mut service = build_query_parsing_service(schema, configuration);
        let request = Request::new("query { me { id } }".to_string(), None);

        let first = service
            .ready()
            .await
            .unwrap()
            .call(request.clone())
            .await
            .unwrap();
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            1u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "miss"
        );

        let second = service.ready().await.unwrap().call(request).await.unwrap();
        assert_eq!(second.hash, first.hash);
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            1u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "miss"
        );
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            1u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "hit"
        );
    }
    .with_metrics()
    .await
}

#[tokio::test]
async fn different_query_is_cache_miss() {
    async {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let mut service = build_query_parsing_service(schema, configuration);

        let a = service
            .ready()
            .await
            .unwrap()
            .call(Request::new("query { me { id } }".to_string(), None))
            .await
            .unwrap();
        let b = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(
                "query { topProducts { name } }".to_string(),
                None,
            ))
            .await
            .unwrap();

        assert_ne!(a.hash, b.hash);
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            2u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "miss"
        );
        assert_histogram_not_exists!(
            "apollo.platform.cache.lookup.duration",
            f64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "hit"
        );
    }
    .with_metrics()
    .await
}

#[tokio::test]
async fn different_operation_name_is_cache_miss() {
    async {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let mut service = build_query_parsing_service(schema, configuration);
        let document = r#"
            query UserId {
                me { id }
            }

            query ProductNames {
                topProducts { name }
            }
        "#;

        let a = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(
                document.to_string(),
                Some("UserId".to_string()),
            ))
            .await
            .unwrap();
        let b = service
            .ready()
            .await
            .unwrap()
            .call(Request::new(
                document.to_string(),
                Some("ProductNames".to_string()),
            ))
            .await
            .unwrap();

        assert_ne!(a.hash, b.hash);
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            2u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "miss"
        );
        assert_histogram_not_exists!(
            "apollo.platform.cache.lookup.duration",
            f64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "hit"
        );
    }
    .with_metrics()
    .await
}

#[tokio::test]
async fn validation_error_is_cached() {
    async {
        let configuration = test_configuration();
        let schema = test_schema(&configuration);
        let mut service = build_query_parsing_service(schema, configuration);
        let request = Request::new("query { me { doesNotExist } }".to_string(), None);

        let err = service
            .ready()
            .await
            .unwrap()
            .call(request.clone())
            .await
            .expect_err("should fail");
        assert!(matches!(err, MaybeBackPressureError::PermanentError(_)));
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            1u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "miss"
        );

        // The error comes from the cache on a second attempt.
        let err = service
            .ready()
            .await
            .unwrap()
            .call(request)
            .await
            .expect_err("should fail again from cache");
        assert!(matches!(err, MaybeBackPressureError::PermanentError(_)));
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            1u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "miss"
        );
        assert_histogram_count!(
            "apollo.platform.cache.lookup.duration",
            1u64,
            "cache.type" = "memory",
            "cache.name" = "query_parsing",
            "cache.status" = "hit"
        );
    }
    .with_metrics()
    .await
}
