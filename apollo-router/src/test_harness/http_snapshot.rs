//! Snapshot server to capture and replay HTTP responses. This is useful for:
//!
//! * Capturing HTTP responses from a real API or server, and replaying them in tests
//! * Mocking responses from a non-existent HTTP API for testing
//! * Working offline by capturing output from a server, and replaying it
//!
//! For example, this can be used with the router `override_subgraph_url` to replay recorded
//! responses from GraphQL subgraphs. Or it can be used with `override_url` in Connectors, to
//! record the HTTP responses from an external REST API. This allows the replayed responses to
//! be used in tests, or even in Apollo Sandbox to work offline or avoid hitting the REST API
//! too frequently.
//!
//! The snapshot server can be started from tests by calling the [`SnapshotServer::spawn`] method,
//! or as a standalone application by invoking [`standalone::main`]. In the latter case, there
//! is a binary wrapper in `http_snapshot_main` that can be run like this:
//!
//! `cargo run --bin snapshot --features="snapshot" -- --snapshot-path <file> --url <base URL to snapshot> [--offline] [--update] [--port <port number>]`
//!
//! Any requests made to the snapshot server will be proxied on to the given base URL, and the
//! responses will be saved to the given file. The next time the snapshot server receives the
//! same request (same relative path, HTTP method, and request body), it will respond with the
//! response recorded in the file rather than sending the request to the upstream server.
//!
//! The snapshot file can be manually edited to manipulate responses for testing purposes, or to
//! redact information that you don't want to include in source-controlled snapshot files.
//!
//! The offline mode will never call the upstream server, and will always return a saved snapshot
//! response. If one is not available, a `500` error is returned. This is useful for tests, for
//! example to ensure that CI builds never attempt to access the network.
//!
//! The update mode can be used to force an update of recorded snapshots, even if there is already
//! a snapshot saved in the file. This overrides the offline mode, and is useful to update tests
//! when a change is made to the upstream HTTP responses.
//!
//! The set of response headers returned can be filtered by supplying a list of headers to include.
//! This is typically desirable, as headers may contain ephemeral information like dates or tokens.
//!
//! **IMPORTANT:** this module stores HTTP responses to the local file system in plain text. It
//! should not be used with production APIs that return sensitive data.
//!
//! This module should also not be used in conjunction with performance testing, as returning
//! snapshot data locally will be much faster than sending HTTP requests to an external server.
use std::net::SocketAddr;
use std::net::TcpListener;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;

use axum::extract::Path as AxumPath;
use axum::extract::State;
use axum::routing::any;
use axum::Router;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Uri;
use hyper::StatusCode;
use hyper_rustls::ConfigBuilderExt;
use indexmap::IndexMap;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use tower::ServiceExt;
use tracing::error;
use tracing::info;

use crate::plugins::traffic_shaping::Http2Config;
use crate::services::http::HttpClientService;
use crate::services::http::HttpRequest;
use crate::services::router::body::RouterBody;

/// An error from the snapshot server
#[derive(Debug, thiserror::Error)]
enum SnapshotError {
    /// Unable to load snapshots
    #[error("unable to load snapshots")]
    IoError(#[from] std::io::Error),
    /// Unable to parse snapshots
    #[error("unable to parse snapshots")]
    ParseError(#[from] serde_json::Error),
}

/// A server that mocks an API using snapshots recorded from actual HTTP responses.
#[cfg_attr(test, allow(unreachable_pub))]
pub struct SnapshotServer {
    // The socket address the server is listening on
    #[cfg_attr(not(test), allow(dead_code))]
    socket_address: SocketAddr,
}

#[derive(Clone)]
struct SnapshotServerState {
    client: HttpClientService,
    base_url: Uri,
    snapshots: Arc<Mutex<Vec<Snapshot>>>,
    snapshot_file: Box<Path>,
    offline: bool,
    update: bool,
    include_headers: Option<Vec<String>>,
}

async fn root_handler(
    State(state): State<SnapshotServerState>,
    req: http::Request<axum::body::Body>,
) -> Result<http::Response<RouterBody>, StatusCode> {
    handle(State(state), req, "/".to_string()).await
}

async fn handler(
    State(state): State<SnapshotServerState>,
    AxumPath(path): AxumPath<String>,
    req: http::Request<axum::body::Body>,
) -> Result<http::Response<RouterBody>, StatusCode> {
    handle(State(state), req, path).await
}

async fn handle(
    State(state): State<SnapshotServerState>,
    req: http::Request<axum::body::Body>,
    path: String,
) -> Result<http::Response<RouterBody>, StatusCode> {
    let uri = [state.base_url.to_string(), path.clone()].concat();
    let method = req.method().clone();
    let version = req.version();
    let request_headers = req.headers().clone();
    let hyper_body = hyper::body::to_bytes(req.into_body()).await.unwrap();
    let router_body: RouterBody = RouterBody::from(hyper_body.clone());
    let request_json_body = serde_json::from_slice(&hyper_body).unwrap_or(Value::Null);

    {
        // check if we have an existing snapshot for this request
        let mut snapshots = state.snapshots.lock().unwrap();
        let existing = snapshots.iter().enumerate().find(|(_, snapshot)| {
            snapshot.request.path == Some(path.clone())
                && snapshot.request.method == Some(method.to_string())
                && snapshot.request.body.clone() == request_json_body
        });
        if let Some((i, snapshot)) = existing {
            if state.update {
                snapshots.remove(i);
            } else {
                info!(
                    url = %uri,
                    method = %method,
                    "Found existing snapshot"
                );
                return Ok(snapshot.clone().try_into().unwrap());
            }
        }
    }

    let error_message;
    if state.offline && !state.update {
        let message = "Offline mode enabled and no snapshot available".to_string();
        error_message = message.clone();
        error!(
            url = %uri,
            method = %method,
            message,
        );
    } else {
        info!(
            url = %uri,
            method = %method,
            "Taking snapshot"
        );
        let mut request = http::Request::builder()
            .method(method.clone())
            .version(version)
            .uri(uri.clone())
            .body(router_body)
            .unwrap();
        *request.headers_mut() = request_headers.clone();
        let response = state
            .client
            .oneshot(HttpRequest {
                http_request: request,
                context: crate::context::Context::new(),
            })
            .await
            .unwrap();
        let (parts, body) = response.http_response.into_parts();

        if let Ok(body_bytes) = body.to_bytes().await {
            if let Ok(response_json_body) = serde_json::from_slice(&body_bytes) {
                let snapshot = Snapshot {
                    request: Request {
                        method: Some(method.to_string()),
                        path: Some(path),
                        body: request_json_body,
                    },
                    response: Response {
                        status: parts.status.as_u16(),
                        headers: map_headers(parts.headers, |name| {
                            state
                                .include_headers
                                .as_ref()
                                .map(|headers| headers.contains(&name.to_string()))
                                .unwrap_or(true)
                        }),
                        body: response_json_body,
                    },
                };
                {
                    let mut snapshots = state.snapshots.lock().unwrap();
                    snapshots.push(snapshot.clone());
                    if let Err(e) = save(state.snapshot_file, snapshots.as_mut()) {
                        error!(
                            url = %uri,
                            method = %method,
                            error = ?e,
                            "Unable to save snapshot"
                        );
                    }
                }
                return Ok(snapshot.try_into().unwrap());
            } else {
                let message = "Unable to parse response body as JSON".to_string();
                error_message = message.clone();
                error!(
                    url = %uri,
                    method = %method,
                    message,
                )
            }
        } else {
            let message = "Unable to read response body".to_string();
            error_message = message.clone();
            error!(
                url = %uri,
                method = %method,
                message
            )
        }
    }

    Ok(http::Response::builder()
        .status(500)
        .body(json!({ "error": error_message}).to_string().into())
        .unwrap())
}

fn map_headers<F: Fn(&str) -> bool>(
    headers: HeaderMap<HeaderValue>,
    include: F,
) -> IndexMap<String, Vec<String>> {
    headers.iter().fold(
        IndexMap::new(),
        |mut map: IndexMap<String, Vec<String>>, (name, value)| {
            let name = name.to_string();
            if include(&name) {
                let value = value.to_str().unwrap_or_default().to_string();
                map.entry(name).or_default().push(value);
            }
            map
        },
    )
}

fn save<P: AsRef<Path>>(path: P, snapshots: &mut Vec<Snapshot>) -> Result<(), SnapshotError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Keep the snapshots sorted to make source control merges easier when updating snapshots
    snapshots.sort_by_key(|snapshot| {
        format!(
            "{}-{}",
            &snapshot
                .request
                .method
                .as_ref()
                .unwrap_or(&String::from("GET")),
            &snapshot.request.path.as_ref().unwrap_or(&String::from("/"))
        )
    });

    std::fs::write(path, serde_json::to_string_pretty(&snapshots)?).map_err(Into::into)
}

fn load<P: AsRef<Path>>(path: P) -> Result<Vec<Snapshot>, SnapshotError> {
    let str = std::fs::read_to_string(path)?;
    let result: Result<Vec<Snapshot>, SnapshotError> =
        serde_json::from_str(&str).map_err(Into::into);
    if let Ok(snapshots) = &result {
        info!("Loaded {} snapshots", snapshots.len());
    }
    result
}

impl SnapshotServer {
    /// Spawn the server in a new task and return. Used for tests.
    #[cfg_attr(test, allow(unreachable_pub))]
    pub async fn spawn<P: AsRef<Path>>(
        snapshot_path: P,
        base_url: Uri,
        offline: bool,
        update: bool,
        include_headers: Option<Vec<String>>,
    ) -> Self {
        Self::inner_start(
            snapshot_path,
            base_url,
            true,
            offline,
            update,
            include_headers,
            None,
        )
        .await
    }

    /// Start the server and block. Can be used to run the server as a standalone application.
    #[cfg(feature = "snapshot")]
    pub(crate) async fn start<P: AsRef<Path>>(
        snapshot_path: P,
        base_url: Uri,
        offline: bool,
        update: bool,
        include_headers: Option<Vec<String>>,
        listener: Option<TcpListener>,
    ) -> Self {
        Self::inner_start(
            snapshot_path,
            base_url,
            false,
            offline,
            update,
            include_headers,
            listener,
        )
        .await
    }

    /// Get the URI the server is listening at
    #[cfg_attr(not(test), allow(dead_code))]
    #[cfg_attr(test, allow(unreachable_pub))]
    pub fn uri(&self) -> String {
        format!("http://{}", self.socket_address)
    }

    async fn inner_start<P: AsRef<Path>>(
        snapshot_path: P,
        base_url: Uri,
        spawn: bool,
        offline: bool,
        update: bool,
        include_headers: Option<Vec<String>>,
        listener: Option<TcpListener>,
    ) -> Self {
        if update {
            info!("Running in update mode ⬆️");
        } else if offline {
            info!("Running in offline mode ⛔️");
        }

        let snapshot_file = snapshot_path.as_ref();
        let snapshots: Vec<Snapshot> = load(snapshot_file).unwrap_or(vec![]);

        let http_service = HttpClientService::new(
            "test",
            Http2Config::Http2Only,
            rustls::ClientConfig::builder()
                .with_safe_defaults()
                .with_native_roots()
                .with_no_client_auth(),
        )
        .expect("can create a HttpService");
        let app = Router::new()
            .route("/", any(root_handler))
            .route("/*path", any(handler))
            .with_state(SnapshotServerState {
                client: http_service,
                base_url: base_url.clone(),
                snapshots: Arc::new(Mutex::new(snapshots.clone())),
                snapshot_file: Box::from(snapshot_file),
                offline,
                update,
                include_headers,
            });
        let listener = listener.unwrap_or(
            TcpListener::bind("127.0.0.1:0")
                .expect("Failed to bind an OS port for snapshot server"),
        );
        let local_address = listener
            .local_addr()
            .expect("Failed to get snapshot server address.");
        info!(
            "Snapshot server listening on port {:?}",
            local_address.port()
        );
        if spawn {
            tokio::spawn(async move {
                axum::Server::from_tcp(listener)
                    .expect("Unable to start snapshot server")
                    .serve(app.into_make_service())
                    .await
                    .unwrap();
            });
        } else {
            axum::Server::from_tcp(listener)
                .expect("Unable to start snapshot server")
                .serve(app.into_make_service())
                .await
                .unwrap();
        }
        Self {
            socket_address: local_address,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    request: Request,
    response: Response,
}

impl TryFrom<Snapshot> for http::Response<RouterBody> {
    type Error = ();

    fn try_from(snapshot: Snapshot) -> Result<Self, Self::Error> {
        let mut response = http::Response::builder().status(snapshot.response.status);
        if let Some(headers) = response.headers_mut() {
            for (name, values) in snapshot.response.headers.into_iter() {
                if let Ok(name) = HeaderName::from_str(&name.clone()) {
                    for value in values {
                        if let Ok(value) = HeaderValue::from_str(&value.clone()) {
                            headers.insert(name.clone(), value);
                        }
                    }
                }
            }
        }
        let body_string = snapshot.response.body.to_string();
        if let Ok(response) = response.body(RouterBody::from(body_string)) {
            return Ok(response);
        }
        Err(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Request {
    method: Option<String>,
    path: Option<String>,
    body: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Response {
    status: u16,
    #[serde(default)]
    headers: IndexMap<String, Vec<String>>,
    body: Value,
}

#[cfg(feature = "snapshot")]
/// Standalone snapshot server
pub(crate) mod standalone {
    use std::net::TcpListener;
    use std::path::PathBuf;

    use clap::Parser;
    use http::Uri;

    use super::SnapshotServer;

    #[derive(Parser, Debug)]
    #[clap(name = "snapshot", about = "Apollo snapshot server")]
    #[command(disable_version_flag(true))]
    struct Args {
        /// Snapshot location relative to the project directory.
        #[arg(short, long, value_parser)]
        snapshot_path: PathBuf,

        /// Base URL for the server.
        #[arg(short = 'l', long, value_parser)]
        url: Uri,

        /// Run in offline mode, without making any HTTP requests to the base URL.
        #[arg(short, long)]
        offline: bool,

        /// Force snapshot updates (overrides `offline`).
        #[arg(short, long)]
        update: bool,

        /// Optional port to listen on (defaults to an ephemeral port).
        #[arg(short, long)]
        port: Option<u16>,
    }

    /// Run the snapshot server as a standalone application
    pub async fn main() {
        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(tracing_core::Level::INFO)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");

        let args = Args::parse();

        let listener = args.port.map(|port| {
            TcpListener::bind(format!("127.0.0.1:{port}"))
                .expect("Failed to bind an OS port for snapshot server")
        });

        SnapshotServer::start(
            args.snapshot_path,
            args.url,
            args.offline,
            args.update,
            None,
            listener,
        )
        .await;
    }
}
