use super::*;
use crate::test_utils::TowerTest;
use tower::ServiceExt;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct TestPartition(String);

#[derive(Clone, Debug)]
struct TestRequest {
    partition_key: String,
    data: String,
}

#[derive(Clone, Debug, PartialEq)]
struct TestResponse {
    data: String,
}

#[tokio::test]
async fn test_partition_service_creation() {
    let service = PartitionService::new(
        |req: &TestRequest| req.partition_key.clone(),
        |_partition_key: String| TowerTest::builder().service(
            |mut handle: tower_test::mock::Handle<TestRequest, TestResponse>| async move {
                handle.allow(1);
                if let Some((req, resp)) = handle.next_request().await {
                    resp.send_response(TestResponse { data: req.data });
                }
            }
        )
    );

    // Service should be created successfully
    assert_eq!(service.cache.len(), 0);
}

#[tokio::test]
async fn test_ergonomic_api_example() {
    // This test demonstrates the exact API requested
    let mut service = PartitionService::new(
        |req: &TestRequest| req.partition_key.clone(),
        |partition_key: String| TowerTest::builder().service(
            move |mut handle: tower_test::mock::Handle<TestRequest, TestResponse>| {
                let pk = partition_key.clone();
                async move {
                    handle.allow(1);
                    if let Some((req, resp)) = handle.next_request().await {
                        resp.send_response(TestResponse { 
                            data: format!("{}:{}", pk, req.data)
                        });
                    }
                }
            }
        )
    );

    let request = TestRequest {
        partition_key: "user123".to_string(),
        data: "hello".to_string(),
    };

    let response = service.ready().await.unwrap().call(request).await.unwrap();
    assert_eq!(response.data, "user123:hello");
}

#[tokio::test]
async fn test_partition_service_with_custom_cache_size() {
    let service = PartitionService::with_cache_size(
        |req: &TestRequest| req.partition_key.clone(),
        |_partition_key: String| TowerTest::builder().service(
            |mut handle: tower_test::mock::Handle<TestRequest, TestResponse>| async move {
                handle.allow(1);
                if let Some((req, resp)) = handle.next_request().await {
                    resp.send_response(TestResponse { data: req.data });
                }
            }
        ),
        500
    );

    // Service should be created successfully with custom cache size
    assert_eq!(service.cache.len(), 0);
}

#[tokio::test]
async fn test_partition_service_caches_services() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    
    let mut service = PartitionService::new(
        |req: &TestRequest| req.partition_key.clone(),
        move |partition_key: String| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            TowerTest::builder().service(
                move |mut handle: tower_test::mock::Handle<TestRequest, TestResponse>| {
                    let partition_data = partition_key.clone();
                    async move {
                        handle.allow(10);
                        while let Some((req, resp)) = handle.next_request().await {
                            resp.send_response(TestResponse {
                                data: format!("{}-{}", partition_data, req.data),
                            });
                        }
                    }
                }
            )
        }
    );

    let req1 = TestRequest {
        partition_key: "partition1".to_string(),
        data: "data1".to_string(),
    };

    let req2 = TestRequest {
        partition_key: "partition1".to_string(), // Same partition
        data: "data2".to_string(),
    };

    // Call the service with the first request
    let response1 = service.ready().await.unwrap().call(req1).await.unwrap();
    assert_eq!(response1.data, "partition1-data1"); // Response includes partition info
    assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should be 1 after first call

    // Call the service with the second request (same partition)
    let response2 = service.ready().await.unwrap().call(req2).await.unwrap();
    assert_eq!(response2.data, "partition1-data2"); // Response includes partition info
    assert_eq!(call_count.load(Ordering::SeqCst), 1); // Should still be 1 (cached)
}

#[tokio::test]
async fn test_different_partitions_create_different_services() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();
    
    let mut service = PartitionService::new(
        |req: &TestRequest| req.partition_key.clone(),
        move |partition_key: String| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            TowerTest::builder().service(
                move |mut handle: tower_test::mock::Handle<TestRequest, TestResponse>| {
                    let partition_data = partition_key.clone();
                    async move {
                        handle.allow(10);
                        while let Some((req, resp)) = handle.next_request().await {
                            resp.send_response(TestResponse {
                                data: format!("{}-{}", partition_data, req.data),
                            });
                        }
                    }
                }
            )
        }
    );

    let req1 = TestRequest {
        partition_key: "partition1".to_string(),
        data: "data1".to_string(),
    };

    let req2 = TestRequest {
        partition_key: "partition2".to_string(), // Different partition
        data: "data2".to_string(),
    };

    // Call with first partition
    let response1 = service.ready().await.unwrap().call(req1).await.unwrap();
    assert_eq!(response1.data, "partition1-data1");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Call with second partition (different)
    let response2 = service.ready().await.unwrap().call(req2).await.unwrap();
    assert_eq!(response2.data, "partition2-data2");
    assert_eq!(call_count.load(Ordering::SeqCst), 2); // Should be 2 (different partitions)
}

#[tokio::test]
async fn test_partition_key_trait_implementation() {
    // Test that our TestPartition implements PartitionKey
    let partition = TestPartition("test".to_string());
    let partition_clone = partition.clone();

    assert_eq!(partition, partition_clone);

    // Test hash consistency
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher1 = DefaultHasher::new();
    let mut hasher2 = DefaultHasher::new();

    partition.hash(&mut hasher1);
    partition_clone.hash(&mut hasher2);

    assert_eq!(hasher1.finish(), hasher2.finish());
}
