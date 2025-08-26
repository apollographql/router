use std::time::Duration;

use apollo_router::services::router::body::RouterBody;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper_util::rt::TokioExecutor;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

// chosen by fair dice roll. guaranteed to be random. https://xkcd.com/221/
const SUBGRAPH_PORT: u16 = 10141; // hard-coded in huge_requests/supergraph.graphql

const SUPERGRAPH_PORT: u16 = 10142; // hard-coded in huge_requests/router.yaml

const VERBOSE: bool = false;

#[tokio::main]
async fn main() {
    println!("Columns:");
    println!("* Size of a String variable in an otherwise small GraphQL request");
    println!("* End-to-end time");
    println!("* Peak RSS (including heaptrack overhead) of a fresh Router process");
    println!();
    for (display, value) in [
        ("  1K", 1_000),
        (" 10K", 10_000),
        ("100K", 100_000),
        ("  1M", 1_000_000),
        (" 10M", 10_000_000),
        ("100M", 100_000_000),
        ("  1G", 1_000_000_000),
    ] {
        print!("{display} ");
        one_request(value).await;
        // Work around "error creating server listener: Address already in use (os error 98)"
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn one_request(string_variable_bytes: usize) {
    let _shutdown_on_drop = spawn_subgraph().await;

    let heaptrack_output = tempfile::NamedTempFile::new().unwrap();
    let heaptrack_output_path = heaptrack_output.path().as_os_str().to_str().unwrap();
    let router_exe = env!("CARGO_BIN_EXE_router");
    let mut child = Command::new("heaptrack")
        .args([
            "-o",
            heaptrack_output_path,
            router_exe,
            "-s",
            "supergraph.graphql",
            "-c",
            "router.yaml",
        ])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("benches")
                .join("huge_requests"),
        )
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
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

    // Warm up Router caches
    graphql_client(1).await;

    let latency = graphql_client(string_variable_bytes).await;
    print!("{:>4} ms ", latency.as_millis());

    // Trigger graceful shutdown by signaling the router process,
    // which is a child of the heaptrack process.
    assert!(
        Command::new("pkill")
            .arg("-P")
            .arg(child.id().unwrap().to_string())
            .arg("-f")
            .arg(router_exe)
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(child.wait().await.unwrap().success());

    let output = Command::new("heaptrack_print")
        // .arg(heaptrack_output_path)
        .arg(format!("{heaptrack_output_path}.zst"))
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(rss) = line.strip_prefix("peak RSS (including heaptrack overhead): ") {
            println!("{rss:>7}")
        }
    }
}

async fn graphql_client(string_variable_bytes: usize) -> Duration {
    let graphql_request = serde_json::json!({
        "query": "mutation Mutation($data: String) { upload(data: $data) }",
        "variables": {"data": "_".repeat(string_variable_bytes)}
    });
    let request = http::Request::post(format!("http://127.0.0.1:{SUPERGRAPH_PORT}"))
        .header("content-type", "application/json")
        .body(serde_json::to_string(&graphql_request).unwrap())
        .unwrap();
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let start_time = std::time::Instant::now();
    let result = client.request(request).await;
    let latency = start_time.elapsed();
    let mut response = result.unwrap();
    let body = response.body_mut().collect().await.unwrap();
    let body = body.to_bytes();
    assert_eq!(
        String::from_utf8_lossy(&body),
        r#"{"data":{"upload":true}}"#
    );
    if VERBOSE {
        println!("{}", String::from_utf8_lossy(&body));
    }
    assert!(response.status().is_success());
    latency
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
    // Read the request body and promptly ignore it
    request
        .into_body()
        .collect()
        .await?
        .to_bytes()
        .iter()
        .for_each(|_chunk| {});
    // Assume we got a GraphQL request with `mutation Mutation { upload($some_string) }`
    let graphql_response = r#"{"data":{"upload":true}}"#;
    Ok::<_, hyper::Error>(http::Response::new(
        Full::new(graphql_response.as_bytes().into())
            .map_err(|never| match never {})
            .boxed_unsync(),
    ))
}

struct ShutdownOnDrop(Option<tokio::sync::mpsc::Sender<()>>);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            drop(tx.send(()));
        }
    }
}
