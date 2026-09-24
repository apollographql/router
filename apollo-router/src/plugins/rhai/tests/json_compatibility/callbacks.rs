//! JSON-bearing fields through real Rhai callbacks at every pipeline stage.
//!
//! Each test drives a plugin service around a mocked inner service. It compares the request
//! body the inner service received and every response chunk the caller received, serialized in
//! order, with Router v2.17.0's behavior. Conversion counts show which fields callbacks
//! converted to Rhai values (`to_rhai`) and wrote back (`from_rhai`).
//!
//! Router-stage callbacks are not covered because Router does not expose request or response
//! bodies as JSON at that stage (router issue #3642).

use std::sync::Arc;

use futures::StreamExt;
use serde_json_bytes::json;
use tower::BoxError;
use tower::ServiceExt;
use tower::util::BoxService;

use crate::Context;
use crate::graphql;
use crate::plugin::DynPlugin;
use crate::plugins::rhai::engine::json_fields::Conversions;
use crate::plugins::rhai::engine::json_fields::FieldCounts;
use crate::plugins::rhai::engine::json_fields::take_conversions;
use crate::services::ExecutionRequest;
use crate::services::SubgraphRequest;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;
use crate::services::subgraph;

/// What crossed the plugin in one call.
#[derive(Debug, PartialEq)]
struct Exchange {
    /// The request body the inner service received, if it was called.
    received: Option<String>,
    /// Every response chunk returned to the caller.
    chunks: Vec<String>,
}

impl Exchange {
    fn new(received: Option<&str>, chunks: &[&str]) -> Self {
        Self {
            received: received.map(str::to_string),
            chunks: chunks.iter().map(|chunk| chunk.to_string()).collect(),
        }
    }
}

async fn plugin(script: &str) -> Box<dyn DynPlugin> {
    crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(&serde_json::json!({
            "scripts": "tests/fixtures/rhai_json_compatibility",
            "main": script,
        }))
        .await
        .unwrap()
}

fn object(value: serde_json_bytes::Value) -> crate::json_ext::Object {
    value.as_object().unwrap().clone()
}

fn graphql_request() -> http::Request<graphql::Request> {
    supergraph_request().supergraph_request
}

fn supergraph_request() -> SupergraphRequest {
    SupergraphRequest::fake_builder()
        .query("{ me }")
        .variables(object(json!({"b": 1, "a": {"y": 2, "x": 1}})))
        .extensions(object(json!({"z": true})))
        .build()
        .unwrap()
}

fn chunk(label: Option<&str>, data: serde_json_bytes::Value) -> graphql::Response {
    graphql::Response::builder()
        .and_label(label.map(str::to_string))
        .data(data)
        .error(graphql::Error::builder().message("original").build())
        .extension("z", true)
        .build()
}

/// A primary response followed by one deferred chunk.
fn primary_and_deferred() -> Vec<graphql::Response> {
    vec![
        chunk(None, json!({"z": "primary", "a": 1})),
        chunk(Some("later"), json!({"z": "deferred", "a": 2})),
    ]
}

fn serialize(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

/// Call a supergraph or execution stage service whose inner service streams `chunks`.
async fn call_streaming<Request: Send + 'static>(
    stage: impl FnOnce(
        BoxService<Request, SupergraphResponse, BoxError>,
    ) -> BoxService<Request, SupergraphResponse, BoxError>,
    request: Request,
    body: fn(&Request) -> &graphql::Request,
    chunks: Vec<graphql::Response>,
) -> Exchange {
    let (mock, mut handle) = tower_test::mock::pair::<Request, SupergraphResponse>();
    let inner = tokio::spawn(async move {
        let (request, responder) = handle.next_request().await?;
        let received = serialize(body(&request));
        responder.send_response(
            SupergraphResponse::fake_stream_builder()
                .responses(chunks)
                .context(Context::new())
                .build()
                .unwrap(),
        );
        Some(received)
    });
    let response = stage(mock.boxed()).oneshot(request).await.unwrap();
    let chunks = response.response.into_body().collect::<Vec<_>>().await;
    Exchange {
        received: inner.await.unwrap(),
        chunks: chunks.iter().map(serialize).collect(),
    }
}

async fn call_supergraph(script: &str, chunks: Vec<graphql::Response>) -> Exchange {
    let plugin = plugin(script).await;
    call_streaming(
        |service| plugin.supergraph_service(service),
        supergraph_request(),
        |request| request.supergraph_request.body(),
        chunks,
    )
    .await
}

async fn call_execution(script: &str, chunks: Vec<graphql::Response>) -> Exchange {
    let plugin = plugin(script).await;
    call_streaming(
        |service| plugin.execution_service(service),
        ExecutionRequest::fake_builder()
            .supergraph_request(graphql_request())
            .build(),
        |request| request.supergraph_request.body(),
        chunks,
    )
    .await
}

async fn call_subgraph(script: &str) -> Exchange {
    let plugin = plugin(script).await;
    let (mock, mut handle) = tower_test::mock::pair::<SubgraphRequest, subgraph::Response>();
    let inner = tokio::spawn(async move {
        let (request, responder) = handle.next_request().await?;
        let received = serialize(request.subgraph_request.body());
        let chunk = chunk(None, json!({"z": "subgraph", "a": 1}));
        responder.send_response(
            subgraph::Response::fake_builder()
                .data(chunk.data.unwrap())
                .errors(chunk.errors)
                .extensions(chunk.extensions)
                .context(request.context)
                .build(),
        );
        Some(received)
    });
    let request = SubgraphRequest::fake_builder()
        .supergraph_request(Arc::new(graphql_request()))
        .subgraph_request(graphql_request())
        .build();
    let response = plugin
        .subgraph_service("accounts", mock.boxed())
        .oneshot(request)
        .await
        .unwrap();
    Exchange {
        received: inner.await.unwrap(),
        chunks: vec![serialize(response.response.body())],
    }
}

/// Conversions performed by one execution of each callback in `stages.rhai`: one per statement
/// that reads and writes back a field, so the two `errors` statements convert `errors` twice.
const REQUEST_EDIT: FieldCounts = FieldCounts {
    request_variables: 1,
    request_extensions: 1,
    response_data: 0,
    response_errors: 0,
    response_extensions: 0,
};
const RESPONSE_EDIT: FieldCounts = FieldCounts {
    request_variables: 0,
    request_extensions: 0,
    response_data: 1,
    response_errors: 2,
    response_extensions: 1,
};

fn times(counts: FieldCounts, n: usize) -> FieldCounts {
    FieldCounts {
        request_variables: counts.request_variables * n,
        request_extensions: counts.request_extensions * n,
        response_data: counts.response_data * n,
        response_errors: counts.response_errors * n,
        response_extensions: counts.response_extensions * n,
    }
}

fn plus(a: FieldCounts, b: FieldCounts) -> FieldCounts {
    FieldCounts {
        request_variables: a.request_variables + b.request_variables,
        request_extensions: a.request_extensions + b.request_extensions,
        response_data: a.response_data + b.response_data,
        response_errors: a.response_errors + b.response_errors,
        response_extensions: a.response_extensions + b.response_extensions,
    }
}

/// Every field edit in one request callback and `responses` response callbacks.
fn edits(responses: usize) -> Conversions {
    let counts = plus(REQUEST_EDIT, times(RESPONSE_EDIT, responses));
    Conversions {
        to_rhai: counts,
        from_rhai: counts,
    }
}

const EDITED_REQUEST: &str = r#"{"query":"{ me }","variables":{"a":{"x":1,"y":2},"added":"variable","b":1},"extensions":{"added":"extension","z":true}}"#;
const EDITED_PRIMARY: &str = r#"{"data":{"a":1,"label":null,"z":"primary"},"errors":[{"message":"original (edited)"},{"message":"added"}],"extensions":{"edited":true,"z":true}}"#;
const EDITED_DEFERRED: &str = r#"{"label":"later","data":{"a":2,"label":"later","z":"deferred"},"errors":[{"message":"original (edited)"},{"message":"added"}],"extensions":{"edited":true,"z":true}}"#;

#[tokio::test]
async fn supergraph_callbacks_edit_request_and_every_response_chunk() {
    take_conversions();
    let exchange = call_supergraph("stages.rhai", primary_and_deferred()).await;
    assert_eq!(
        exchange,
        Exchange::new(Some(EDITED_REQUEST), &[EDITED_PRIMARY, EDITED_DEFERRED])
    );
    assert_eq!(take_conversions(), edits(2));
}

#[tokio::test]
async fn execution_callbacks_edit_request_and_every_response_chunk() {
    take_conversions();
    let exchange = call_execution("stages.rhai", primary_and_deferred()).await;
    assert_eq!(
        exchange,
        Exchange::new(Some(EDITED_REQUEST), &[EDITED_PRIMARY, EDITED_DEFERRED])
    );
    assert_eq!(take_conversions(), edits(2));
}

#[tokio::test]
async fn subgraph_callbacks_edit_outgoing_request_and_response() {
    take_conversions();
    let exchange = call_subgraph("stages.rhai").await;
    assert_eq!(
        exchange,
        Exchange::new(
            Some(
                r#"{"query":"{ me }","variables":{"a":{"x":1,"y":2},"added":"variable","b":1},"extensions":{"added":"extension","z":true}}"#
            ),
            &[
                r#"{"data":{"a":1,"label":null,"z":"subgraph"},"errors":[{"message":"original (edited)"},{"message":"added"}],"extensions":{"edited":true,"z":true}}"#
            ],
        )
    );
    // The discarded edit to the read-only originating request still converts its variables.
    let mut expected = edits(1);
    expected.to_rhai.request_variables += 1;
    expected.from_rhai.request_variables += 1;
    assert_eq!(take_conversions(), expected);
}

#[tokio::test]
async fn callbacks_that_never_touch_json_convert_nothing() {
    take_conversions();
    call_supergraph("no_access.rhai", primary_and_deferred()).await;
    call_execution("no_access.rhai", primary_and_deferred()).await;
    call_subgraph("no_access.rhai").await;
    assert_eq!(take_conversions(), Conversions::default());
}

#[tokio::test]
async fn primary_edit_then_throw_replaces_the_response() {
    take_conversions();
    let exchange = call_supergraph(
        "edit_then_throw.rhai",
        vec![chunk(None, json!({"fail": true}))],
    )
    .await;
    assert_eq!(
        exchange,
        Exchange::new(
            Some(
                r#"{"query":"{ me }","variables":{"b":1,"a":{"y":2,"x":1}},"extensions":{"z":true}}"#
            ),
            &[
                r#"{"errors":[{"message":"rhai execution error: 'Runtime error: response failure (line 7, position 13)'"}]}"#
            ],
        )
    );
    // Only `data` converts: once for the edit and once for the `fail` check, which reads without
    // writing back. The untouched `errors` and `extensions` never convert.
    let data_only = FieldCounts {
        response_data: 2,
        ..FieldCounts::default()
    };
    assert_eq!(
        take_conversions(),
        Conversions {
            to_rhai: data_only,
            from_rhai: FieldCounts {
                response_data: 1,
                ..FieldCounts::default()
            },
        }
    );
}

#[tokio::test]
async fn deferred_edit_then_throw_keeps_the_edit_and_replaces_errors() {
    let exchange = call_supergraph(
        "edit_then_throw.rhai",
        vec![
            chunk(None, json!({"fail": false})),
            chunk(Some("later"), json!({"fail": true})),
        ],
    )
    .await;
    assert_eq!(
        exchange.chunks,
        [
            r#"{"data":{"edited":true,"fail":false},"errors":[{"message":"original"}],"extensions":{"z":true}}"#,
            r#"{"label":"later","data":{"edited":true,"fail":true},"errors":[{"message":"rhai execution error: 'Runtime error: response failure (line 7, position 13)'"}],"extensions":{"z":true}}"#,
        ]
    );
}

#[tokio::test]
async fn request_edit_then_throw_never_reaches_the_inner_service() {
    let exchange = call_supergraph("request_edit_then_throw.rhai", primary_and_deferred()).await;
    assert_eq!(
        exchange,
        Exchange::new(
            None,
            &[
                r#"{"errors":[{"message":"rhai execution error: 'Runtime error: request failure (line 6, position 9)'"}]}"#
            ],
        )
    );
}

#[tokio::test]
async fn later_callbacks_observe_earlier_edits() {
    // Each `map_request` wraps the service registered before it, so the last-registered request
    // callback runs first, while response callbacks run in registration order. Either way the
    // callback that runs later observes the earlier callback's write-back.
    let exchange = call_supergraph("later_callbacks.rhai", primary_and_deferred()).await;
    assert_eq!(
        exchange,
        Exchange::new(
            Some(
                r#"{"query":"{ me }","variables":{"a":{"x":1,"y":2},"b":1,"first_ran":true,"first_saw_second":true,"second_ran":true,"second_saw_first":null},"extensions":{"z":true}}"#
            ),
            &[
                r#"{"data":{"a":1,"first_ran":true,"first_saw_second":null,"second_ran":true,"second_saw_first":true,"z":"primary"},"errors":[{"message":"original"}],"extensions":{"z":true}}"#,
                r#"{"label":"later","data":{"a":2,"first_ran":true,"first_saw_second":null,"second_ran":true,"second_saw_first":true,"z":"deferred"},"errors":[{"message":"original"}],"extensions":{"z":true}}"#,
            ],
        )
    );
}

#[tokio::test]
async fn dropping_the_response_stream_skips_remaining_deferred_callbacks() {
    let plugin = plugin("stages.rhai").await;
    let (mock, mut handle) = tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
    let inner = tokio::spawn(async move {
        let (_, responder) = handle.next_request().await.unwrap();
        let mut chunks = primary_and_deferred();
        chunks.push(chunk(Some("never read"), json!({})));
        responder.send_response(
            SupergraphResponse::fake_stream_builder()
                .responses(chunks)
                .context(Context::new())
                .build()
                .unwrap(),
        );
    });
    take_conversions();
    let response = plugin
        .supergraph_service(mock.boxed())
        .oneshot(supergraph_request())
        .await
        .unwrap();
    inner.await.unwrap();
    let mut stream = response.response.into_body();
    assert_eq!(serialize(&stream.next().await.unwrap()), EDITED_PRIMARY);
    drop(stream);
    assert_eq!(take_conversions(), edits(1));
}

#[tokio::test]
async fn cancelling_a_request_skips_response_callbacks() {
    let plugin = plugin("stages.rhai").await;
    let (mock, mut handle) = tower_test::mock::pair::<SupergraphRequest, SupergraphResponse>();
    take_conversions();
    let call = plugin
        .supergraph_service(mock.boxed())
        .oneshot(supergraph_request());
    tokio::pin!(call);
    let (received, _responder) = tokio::select! {
        received = handle.next_request() => received.unwrap(),
        _ = &mut call => panic!("the call completed before reaching the inner service"),
    };
    assert_eq!(
        serialize(received.supergraph_request.body()),
        EDITED_REQUEST
    );
    drop(call);
    assert_eq!(take_conversions(), edits(0));
}
