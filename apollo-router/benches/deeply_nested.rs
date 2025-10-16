//! Exercise the Router, compiled in release mode, with very deeply-nested selections sets
//! and response data.
//!
//! Run with `cargo bench --bench deeply_nested`

#![allow(clippy::single_char_add_str)] // don’t care

use std::fmt::Write;
use std::time::Duration;

use apollo_router::services::router::body::RouterBody;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper_util::rt::TokioExecutor;
use serde_json_bytes::Value;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

const ROUTER_EXE: &str = env!("CARGO_BIN_EXE_router");

// chosen by fair dice roll. guaranteed to be random. https://xkcd.com/221/
const SUBGRAPH_PORT: u16 = 44168; // hard-coded in deeply_nested/supergraph.graphql

const SUPERGRAPH_PORT: u16 = 44167; // hard-coded in deeply_nested/router.yaml

const VERBOSE: bool = false;

#[tokio::main]
async fn main() {
    if VERBOSE {
        println!("Router executable: {ROUTER_EXE}");
    }
    assert!(ROUTER_EXE.contains("release"));
    macro_rules! request {
        ($nesting_level: expr) => {{
            let level = $nesting_level;
            let result = graphql_client(level).await;
            if let Err(error) = &result {
                if VERBOSE {
                    if error.len() < 200 {
                        println!("Error at {level} nesting levels: {error}\n");
                    } else {
                        println!(
                            "Error at {level} nesting levels: {}[…]{}\n",
                            &error[..100],
                            &error[error.len() - 100..]
                        );
                    }
                }
            }
            result
        }};
    }

    let _subgraph = spawn_subgraph().await;

    let graphql_recursion_limit = 5_000;
    let _router = spawn_router(graphql_recursion_limit).await;

    assert_eq!(
        request!(8).unwrap().to_string(),
        r#"{"value":0,"next":{"value":1,"next":{"value":1,"next":{"value":2,"next":{"value":3,"next":{"value":5,"next":{"value":8,"next":{"value":13,"next":{"value":21}}}}}}}}}"#
    );

    assert!(request!(125).is_ok());

    // JSON parser recursion limit in serde_json::Deserializier
    assert!(
        request!(126)
            .unwrap_err()
            .contains("service 'subgraph_1' response was malformed: recursion limit exceeded")
    );

    // Stack overflow: the router process aborts before it can send a response
    //
    // As of commit 6e426ddf2fe9480210dfa74c85040db498c780a2 (Router 1.33.2+),
    // with Rust 1.72.0 on aarch64-apple-darwin, this happens starting at ~2400 nesting levels.
    assert!(
        request!(3000)
            .unwrap_err()
            .contains("connection closed before message completed")
    );

    let graphql_recursion_limit = 500;
    let _router = spawn_router(graphql_recursion_limit).await;

    // GraphQL parser recursion limit in apollo-parser
    assert!(
        request!(500)
            .unwrap_err()
            .contains("Error: parser recursion limit reached")
    );
}

async fn spawn_router(graphql_recursion_limit: usize) -> tokio::process::Child {
    let mut child = Command::new(ROUTER_EXE)
        .args(["-s", "supergraph.graphql", "-c", "router.yaml"])
        .env("PARSER_MAX_RECURSION", graphql_recursion_limit.to_string())
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("benches")
                .join("deeply_nested"),
        )
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(if VERBOSE {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        })
        .spawn()
        .unwrap();

    let mut router_stdout = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut tx = Some(tx);
        while let Some(line) = router_stdout.next_line().await.unwrap() {
            if line.contains("GraphQL endpoint exposed")
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(());
                // Don’t stop here, keep consuming output so the pipe doesn’t block on a full buffer
            }
            if VERBOSE {
                println!("{line}");
            }
        }
    });
    rx.await.unwrap();
    child
}

async fn graphql_client(nesting_level: usize) -> Result<Value, String> {
    let mut json = String::from(r#"{"query":"{value"#);
    for _ in 0..nesting_level {
        json.push_str(" next{value");
    }
    for _ in 0..nesting_level {
        json.push_str("}");
    }
    json.push_str(r#"}"}"#);
    let request = http::Request::post(format!("http://127.0.0.1:{SUPERGRAPH_PORT}"))
        .header("content-type", "application/json")
        .header("fibonacci-iterations", nesting_level)
        .body(json)
        .unwrap();
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let mut response = client.request(request).await.map_err(|e| e.to_string())?;
    let body = response
        .body_mut()
        .collect()
        .await
        .map_err(|e| e.to_string())?;
    let json = serde_json::from_slice::<Value>(&body.to_bytes()).map_err(|e| e.to_string())?;
    if let Some(errors) = json.get("errors")
        && !errors.is_null()
    {
        return Err(errors.to_string());
    }
    Ok(json.get("data").cloned().unwrap_or(Value::Null))
}

async fn spawn_subgraph() -> ShutdownOnDrop {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(2);
    let shutdown_on_drop = ShutdownOnDrop(Some(tx));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", SUBGRAPH_PORT))
        .await
        .unwrap();
    let server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                conn = listener.accept() => {
                    let (stream, peer_addr) = conn.unwrap();
                    let stream = hyper_util::rt::TokioIo::new(Box::pin(stream));
                    let conn = server
                        .serve_connection_with_upgrades(stream, hyper::service::service_fn(subgraph));
                    let conn = graceful.watch(conn.into_owned());

                    tokio::spawn(async move {
                        if let Err(err) = conn.await {
                            eprintln!("connection error: {err}");
                        }
                        eprintln!("connection dropped: {peer_addr}");
                    });
                }
                _ = rx.recv() => {
                    drop(listener);
                    break;
                }
            }
        }

        tokio::select! {
            _ = graceful.shutdown() => {
                eprintln!("Gracefully shutdown!");
            },
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                eprintln!("Waited 10 seconds for graceful shutdown, aborting...");
            }
        }
    });

    shutdown_on_drop
}

async fn subgraph(
    request: http::Request<hyper::body::Incoming>,
) -> Result<http::Response<RouterBody>, hyper::Error> {
    let nesting_level = request
        .headers()
        .get("fibonacci-iterations")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    // Read the request body and promptly ignore it
    request
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .into_iter()
        .for_each(|_chunk| {});
    // Assume we got a GraphQL request with that many nested selection sets
    let mut json = String::from(r#"{"data":{"value":0"#);
    let mut a = 1;
    let mut b = 1;
    for _ in 0..nesting_level {
        json.push_str(r#","next":{"value":"#);
        write!(&mut json, "{a}").unwrap();
        let next = a + b;
        a = b;
        b = next;
    }
    for _ in 0..nesting_level {
        json.push_str("}");
    }
    json.push_str("}}");
    let mut response = http::Response::new(
        Full::new(json.into())
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    let application_json = hyper::header::HeaderValue::from_static("application/json");
    response
        .headers_mut()
        .insert("content-type", application_json);
    Ok(response)
}

struct ShutdownOnDrop(Option<tokio::sync::mpsc::Sender<()>>);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            drop(tx.send(()));
        }
    }
}
