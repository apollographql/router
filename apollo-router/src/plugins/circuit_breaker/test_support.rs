//! A check, shared by the plugins that answer subgraph requests themselves, that their answers
//! record no outcome on the subgraph's circuit.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use tower::Layer;
use tower::ServiceExt;

use super::CircuitBreaker;
use super::Error;
use crate::graphql;
use crate::plugins::test::PluginTestHarness;
use crate::services::subgraph;

/// How many requests [`assert_router_answers_record_nothing`] sends for the router to answer.
const BURST: usize = 5;

/// A request to subgraph `products` for an operation named `operation_name`.
pub(crate) fn subgraph_request(operation_name: &str) -> subgraph::Request {
    subgraph::Request::fake_builder()
        .subgraph_name("products")
        .subgraph_request(
            http::Request::builder()
                .body(
                    graphql::Request::fake_builder()
                        .query("query { hello }")
                        .operation_name(operation_name)
                        .build(),
                )
                .unwrap(),
        )
        .build()
}

/// Checks that the subgraph requests `answering` answers itself record no outcome on the
/// subgraph's circuit.
///
/// `answering` wraps subgraph `products`, which fails every request it receives with a `500`, and
/// sits behind the subgraph's circuit, which opens after two failures in a row. The check sends
/// one request from `sent_on`, then [`BURST`] from `answered`, then two more from `sent_on`. An
/// answer recorded as a failure would open the circuit during the burst, and one recorded as a
/// success would reset the count of failures, so the circuit would still be closed at the end.
pub(crate) async fn assert_router_answers_record_nothing(
    answering: impl AsyncFnOnce(subgraph::BoxCloneService) -> subgraph::BoxCloneService,
    sent_on: impl Fn() -> subgraph::Request,
    answered: impl Fn() -> subgraph::Request,
) {
    let circuit_breaker = PluginTestHarness::<CircuitBreaker>::builder()
        .config(
            r#"
            circuit_breaker:
              all:
                consecutive_failures: 2
            "#,
        )
        .build()
        .await
        .expect("plugin should be configured");

    let calls = Arc::new(AtomicUsize::new(0));
    let subgraph = {
        let calls = calls.clone();
        tower::service_fn(move |request: subgraph::Request| {
            calls.fetch_add(1, Ordering::SeqCst);
            let response = subgraph::Response::fake_builder()
                .status_code(http::StatusCode::INTERNAL_SERVER_ERROR)
                .context(request.context)
                .id(request.id)
                .build();
            async move { Ok(response) }
        })
        .boxed_clone()
    };
    let service = circuit_breaker
        .subgraph_circuit_layer("products")
        .layer(answering(subgraph).await)
        .boxed_clone();
    let send = async |request| {
        let response = service.clone().oneshot(request).await.expect("answered");
        response.response.body().errors.first()?.extension_code()
    };
    let open = Some(Error::CircuitBreakerOpen.code().to_string());

    send(sent_on()).await;
    for _ in 0..BURST {
        assert_ne!(
            send(answered()).await,
            open,
            "the circuit should still be closed"
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the router should have answered the burst without calling the subgraph"
    );
    assert_ne!(send(sent_on()).await, open);
    assert_eq!(
        send(sent_on()).await,
        open,
        "the failures either side of the burst should have opened the circuit"
    );
}
