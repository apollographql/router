/*
mod report {
    tonic::include_proto!("report");
}
*/

use prost_types::Timestamp;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use usage_agent::report::trace::CachePolicy;
use usage_agent::report::{Report, Trace, TracesAndStats};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut reporter = usage_agent::Reporter::try_new("https://127.0.0.1:50051").await?;

    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let seconds = time.as_secs();
    let nanos = time.as_nanos() - (seconds as u128 * 1_000_000_000);
    let ts = Timestamp {
        seconds: seconds as i64,
        nanos: nanos as i32,
    };
    let mut tpq = HashMap::new();

    let start_time = ts.clone();
    let mut end_time = ts.clone();
    end_time.nanos += 100;
    let trace = Trace {
        start_time: Some(start_time),
        end_time: Some(end_time),
        duration_ns: 100,
        cache_policy: Some(CachePolicy {
            scope: 0,
            max_age_ns: 0,
        }),
        ..Default::default()
    };
    let tns = TracesAndStats {
        trace: vec![trace],
        ..Default::default()
    };
    tpq.insert(
        "# query ExampleQuery {
  topProducts {
    name
  }
}"
        .to_string(),
        tns,
    );
    println!("tpq: {:?}", tpq);
    let mut report = Report::try_new("Usage-Agent-uc0sri@current")?;
    report.traces_per_query = tpq;
    report.end_time = Some(ts);

    let response = reporter.submit(report).await?;
    println!("response: {}", response.into_inner().message);

    Ok(())
}
