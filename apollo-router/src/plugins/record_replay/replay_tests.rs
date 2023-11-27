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

    let report = report.lock().unwrap();
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
