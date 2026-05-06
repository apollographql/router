use std::time::Duration;
use std::time::Instant;

use insta::assert_yaml_snapshot;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use tower::BoxError;

use crate::integration::common::IntegrationTest;
use crate::integration::common::Query;
use crate::integration::common::graph_os_enabled;

#[tokio::test(flavor = "multi_thread")]
async fn test_standard_events() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = EventTest::new(json!({
      "router": { "request": "info", "response": "info" },
      "supergraph": { "request": "info", "response": "info" },
      "subgraph": { "request": "info", "response": "info" }
    }))
    .await;

    assert_yaml_snapshot!(router.execute_default_query().await?, @r"
    - kind: router.request
      level: INFO
    - kind: supergraph.request
      level: INFO
    - kind: subgraph.request
      level: INFO
    - kind: subgraph.response
      level: INFO
    - kind: supergraph.response
      level: INFO
    - kind: router.response
      level: INFO
    ");

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_custom_events() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let mut router = EventTest::new(json!({
      "router": {
        "custom.router.request": {
          "on": "request",
          "message": "Custom router request",
          "level": "info"
        },
        "custom.router.response": {
          "on": "response",
          "message": "Custom router response",
          "level": "info"
        }
      },
      "supergraph": {
        "custom.supergraph.request": {
          "on": "request",
          "message": "Custom supergraph request",
          "level": "info"
        },
        "custom.supergraph.response": {
          "on": "response",
          "message": "Custom supergraph response",
          "level": "info"
        }
      },
      "subgraph": {
        "custom.subgraph.request": {
          "on": "request",
          "message": "Custom subgraph request",
          "level": "info"
        },
        "custom.subgraph.response": {
          "on": "response",
          "message": "Custom subgraph response",
          "level": "info"
        }
      }
    }))
    .await;

    assert_yaml_snapshot!(router.execute_default_query().await?, @r"
    - kind: custom.router.request
      level: INFO
    - kind: custom.supergraph.request
      level: INFO
    - kind: custom.subgraph.request
      level: INFO
    - kind: custom.subgraph.response
      level: INFO
    - kind: custom.supergraph.response
      level: INFO
    - kind: custom.router.response
      level: INFO
    ");

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_events_with_request_header_condition() -> Result<(), BoxError> {
    if !graph_os_enabled() {
        eprintln!("test skipped");
        return Ok(());
    }

    let x_log_request = String::from("x-log-request");
    let x_log_response = String::from("x-log-response");
    let x_log_custom_request = String::from("x-log-custom-request");
    let x_log_custom_response = String::from("x-log-custom-response");

    let mut router = EventTest::new(json!({
      "router": {
        "request": {
          "level": "info",
          "condition": { "exists": { "request_header": x_log_request } }
        },
        "response": {
          "level": "info",
          "condition": { "exists": { "request_header": x_log_response } }
        },
        "custom.router.request": {
          "on": "request",
          "message": "Custom router request",
          "level": "info",
          "condition": { "exists": { "request_header": x_log_custom_request } }
        },
        "custom.router.response": {
          "on": "response",
          "message": "Custom router response",
          "level": "info",
          "condition": { "exists": { "request_header": x_log_custom_response } }
        }
      },
      "supergraph": {
        "request": {
          "level": "info",
          "condition": { "exists": { "request_header": x_log_request } }
        },
        "response": {
          "level": "info",
          "condition": { "exists": { "request_header": x_log_response } }
        },
        "custom.supergraph.request": {
          "on": "request",
          "message": "Custom supergraph request",
          "level": "info",
          "condition": { "exists": { "request_header": x_log_custom_request } }
        },
        "custom.supergraph.response": {
          "on": "response",
          "message": "Custom supergraph response",
          "level": "info",
          "condition": { "exists": { "request_header": x_log_custom_response } }
        }
      },
      "subgraph": {
        "request": {
          "level": "info",
          "condition": { "exists": { "subgraph_request_header": x_log_request } }
        },
        "response": {
          "level": "info",
          "condition": { "exists": { "subgraph_request_header": x_log_response } }
        },
        "custom.subgraph.request": {
          "on": "request",
          "message": "Custom subgraph request",
          "level": "info",
          "condition": { "exists": { "subgraph_request_header": x_log_custom_request } }
        },
        "custom.subgraph.response": {
          "on": "response",
          "message": "Custom subgraph response",
          "level": "info",
          "condition": { "exists": { "subgraph_request_header": x_log_custom_response } }
        }
      }
    }))
    .await;

    assert_yaml_snapshot!(router.execute_default_query().await?, @r"[]");

    let query = Query::builder()
        .header(x_log_request.clone(), "enabled".to_owned())
        .build();
    assert_yaml_snapshot!(router.execute_query(query).await?, @r"
    - kind: router.request
      level: INFO
    - kind: supergraph.request
      level: INFO
    - kind: subgraph.request
      level: INFO
    ");

    let query = Query::builder()
        .header(x_log_response.clone(), "enabled".to_owned())
        .build();
    assert_yaml_snapshot!(router.execute_query(query).await?, @r"
    - kind: subgraph.response
      level: INFO
    - kind: supergraph.response
      level: INFO
    - kind: router.response
      level: INFO
    ");

    let query = Query::builder()
        .header(x_log_custom_request.clone(), "enabled".to_owned())
        .build();
    assert_yaml_snapshot!(router.execute_query(query).await?, @r"
    - kind: custom.router.request
      level: INFO
    - kind: custom.supergraph.request
      level: INFO
    - kind: custom.subgraph.request
      level: INFO
    ");

    let query = Query::builder()
        .header(x_log_custom_response.clone(), "enabled".to_owned())
        .build();
    assert_yaml_snapshot!(router.execute_query(query).await?, @r"
    - kind: custom.subgraph.response
      level: INFO
    - kind: custom.supergraph.response
      level: INFO
    - kind: custom.router.response
      level: INFO
    ");

    let query = Query::builder()
        .header(x_log_request.clone(), "enabled".to_owned())
        .header(x_log_response.clone(), "enabled".to_owned())
        .header(x_log_custom_request.clone(), "enabled".to_owned())
        .header(x_log_custom_response.clone(), "enabled".to_owned())
        .build();
    assert_yaml_snapshot!(router.execute_query(query).await?, @r"
    - kind: custom.router.request
      level: INFO
    - kind: router.request
      level: INFO
    - kind: supergraph.request
      level: INFO
    - kind: custom.supergraph.request
      level: INFO
    - kind: custom.subgraph.request
      level: INFO
    - kind: subgraph.request
      level: INFO
    - kind: subgraph.response
      level: INFO
    - kind: custom.subgraph.response
      level: INFO
    - kind: custom.supergraph.response
      level: INFO
    - kind: supergraph.response
      level: INFO
    - kind: router.response
      level: INFO
    - kind: custom.router.response
      level: INFO
    ");

    router.graceful_shutdown().await;
    Ok(())
}

#[derive(Serialize, Deserialize, Eq, PartialEq, Debug)]
struct EventLog {
    kind: String,
    level: String,
}

struct EventTest {
    router: IntegrationTest,
}

impl EventTest {
    async fn new(events_config: serde_json::Value) -> Self {
        let config = json!({
            "headers": {
              "all": {
                "request": [{ "propagate": { "matching": "x-log-.*" } }]
              }
            },
            "telemetry": {
                "instrumentation": {
                    "events": events_config
                },
                "exporters": {
                    "logging": {
                        "stdout": {
                            "enabled": true,
                        }
                    }
                }
            }
        });
        let mut router = IntegrationTest::builder()
            .config(serde_yaml::to_string(&config).expect("valid yaml"))
            .build()
            .await;
        router.start().await;
        router.assert_started().await;

        Self { router }
    }

    async fn execute_default_query(&mut self) -> Result<Vec<EventLog>, BoxError> {
        self.router.read_logs();

        let (_, response) = self.router.execute_default_query().await;
        response.error_for_status()?;

        Ok(self.capture_logged_events().await)
    }

    async fn execute_query(&mut self, query: Query) -> Result<Vec<EventLog>, BoxError> {
        self.router.read_logs();

        let (_, response) = self.router.execute_query(query).await;
        response.error_for_status()?;

        Ok(self.capture_logged_events().await)
    }

    // Drain the router's stdout channel for a deadline-bounded window after
    // the HTTP response returns, since the `router.response` /
    // `supergraph.response` events are emitted *after* the response is
    // written to the wire. Returning immediately on the first
    // `try_recv` miss (the previous behaviour) caused
    // `test_standard_events` to flake whenever the test thread reached
    // `capture_logs` before all six events had crossed the
    // `stdio_rx` channel — same shape as the
    // `test_updown_counter_temporality_with_delta` miss in
    // `blog-details.md` F4.
    //
    // Heuristic: poll until `quiet` ms pass with no new events, capped
    // at `deadline`. Default tuning is conservative (200ms quiet, 5s
    // deadline) — the events arrive within sub-millisecond of each
    // other once the router's tracing layer flushes them, so 200ms is
    // already well past the worst observed batch interval.
    async fn capture_logged_events(&mut self) -> Vec<EventLog> {
        let deadline = Duration::from_secs(5);
        let quiet = Duration::from_millis(200);
        let start = Instant::now();
        // `last_event` is `None` until at least one event has been
        // observed. Initializing it as `Some(Instant::now())` at function
        // entry would make the `last_event.elapsed() >= quiet` check fire
        // at T+200 ms relative to entry whenever no events have arrived
        // yet — which would convert the 5 s deadline into an unreachable
        // upper bound for the cold-start case (the exact scenario this
        // function exists to handle).
        let mut last_event: Option<Instant> = None;
        let mut events: Vec<EventLog> = Vec::new();
        loop {
            let new = self
                .router
                .capture_logs(|s| serde_json::from_str::<EventLog>(&s).ok());
            if !new.is_empty() {
                events.extend(new);
                last_event = Some(Instant::now());
            }
            let quiet_reached = last_event.is_some_and(|t| t.elapsed() >= quiet);
            if quiet_reached || start.elapsed() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        events
    }

    async fn graceful_shutdown(&mut self) {
        self.router.graceful_shutdown().await
    }
}

impl Drop for EventTest {
    fn drop(&mut self) {}
}
