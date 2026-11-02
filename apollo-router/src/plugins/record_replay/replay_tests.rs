#![cfg(test)]

use std::path::Path;

use console::style;
use tower::ServiceExt;

use super::super::replay::Replay;
use crate::TestHarness;

#[tokio::test]
async fn replay_recording() {
    let recording_file = if let Ok(file) = std::env::var("RECORDING_FILE") {
        file
    } else {
        eprintln!("No recording file to replay");
        return;
    };

    let replay = Replay::from_file(Path::new(&recording_file)).await.unwrap();

    let req = replay.make_client_request().unwrap();
    let report = replay.report.clone();

    let test_harness = TestHarness::builder()
        .schema(&replay.supergraph_sdl())
        .extra_plugin(replay)
        .build_router()
        .await
        .unwrap();

    let mut resp = test_harness.oneshot(req).await.unwrap();
    while (resp.next_response().await).is_some() {}

    let report = report.lock();
    let has_items = report.len();

    if has_items == 0 {
        println!("{}", style("Replay matched the recording 🎉").green());
    } else {
        println!();
        for item in report.iter() {
            item.print();
            println!();
        }
    }
}

/// A replayed subgraph response comes from the recording, not the subgraph, so the subgraph's
/// circuit records nothing for it.
#[tokio::test]
async fn a_replayed_subgraph_response_records_no_circuit_outcome() {
    use super::super::recording::Recording;
    use crate::plugins::circuit_breaker::test_support::assert_router_answers_record_nothing;
    use crate::plugins::circuit_breaker::test_support::subgraph_request;

    let request = serde_json::json!({
        "query": "query Recorded { hello }",
        "operation_name": "Recorded",
        "variables": {},
        "headers": {},
        "header_errors": {},
        "method": "POST",
        "uri": "http://products/",
    });
    let response = serde_json::json!({
        "chunks": [{ "data": { "hello": "recorded" } }],
        "headers": {},
        "header_errors": {},
    });
    let recording: Recording = serde_json::from_value(serde_json::json!({
        "supergraph_sdl": "",
        "client_request": request,
        "client_response": response,
        "formatted_query_plan": null,
        "subgraph_fetches": {
            "Recorded": {
                "subgraph_name": "products",
                "request": request,
                "response": response,
            },
        },
    }))
    .expect("the recording parses");
    let replay = Replay::new(recording);

    assert_router_answers_record_nothing(
        async |subgraph| crate::plugin::Plugin::subgraph_service(&replay, "products", subgraph),
        || subgraph_request("Live"),
        || subgraph_request("Recorded"),
    )
    .await;
}
