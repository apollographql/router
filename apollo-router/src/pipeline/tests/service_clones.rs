//! Per-request clones of the service tree, counted through the router built by [`TestHarness`].
//!
//! `BoxCloneService::clone` is deep: it clones every boxed layer beneath it, down to the next
//! `Buffer`. A layer that moves a clone of its inner service into its response future therefore
//! clones the whole tree under it on every request. Only layers that must `.await` before calling
//! their inner service should pay that; layers that decide synchronously call the inner service
//! they were readied with.
//!
//! [`CountClones`] sits at the bottom of the plugin hooks of the supergraph and execution stages,
//! so it counts every clone of a tree that contains it. Traffic shaping's router-stage `Buffer`
//! stops clones from the layers above it, so those are not counted.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use futures::StreamExt;
use tower::ServiceExt;

use crate::TestHarness;
use crate::plugin::test::CountClones;
use crate::services::supergraph;

#[tokio::test]
async fn a_request_clones_the_service_tree_only_in_layers_that_await() {
    let supergraph_clones = Arc::new(AtomicUsize::new(0));
    let execution_clones = Arc::new(AtomicUsize::new(0));

    let router = TestHarness::builder()
        .supergraph_hook({
            let clones = supergraph_clones.clone();
            move |service| CountClones::new(service, clones.clone()).boxed_clone()
        })
        .execution_hook({
            let clones = execution_clones.clone();
            move |service| CountClones::new(service, clones.clone()).boxed_clone()
        })
        .build_router()
        .await
        .unwrap();

    // Only count what serving a request does, not what building the stack did.
    supergraph_clones.store(0, Ordering::Relaxed);
    execution_clones.store(0, Ordering::Relaxed);

    let request = supergraph::Request::canned_builder().build().unwrap();
    let mut responses = router
        .oneshot(request.try_into().unwrap())
        .await
        .unwrap()
        .into_graphql_response_stream()
        .await;
    let response = responses.next().await.unwrap().unwrap();
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert!(responses.next().await.is_none());

    // Router stage, POST request: `SplitBatchRequestLayer` and `RouterToSupergraphRequestLayer`
    // await the request body, and `ParseQueryLayer` awaits the query parsing service.
    assert_eq!(
        supergraph_clones.load(Ordering::Relaxed),
        3,
        "a router or supergraph layer above the plugin hooks cloned its inner service"
    );
    // Plus `SupergraphService`, which awaits the query planner before it calls the execution
    // service.
    assert_eq!(
        execution_clones.load(Ordering::Relaxed),
        4,
        "a supergraph or execution layer cloned its inner service"
    );
}
