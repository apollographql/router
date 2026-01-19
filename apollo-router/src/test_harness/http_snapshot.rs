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
//! `cargo run --bin snapshot --features snapshot -- --snapshot-path <file> --url <base URL to snapshot> [--offline] [--update] [--port <port number>] [-v]`
//!
//! Any requests made to the snapshot server will be proxied on to the given base URL, and the
//! responses will be saved to the given file. The next time the snapshot server receives the
//! same request, it will respond with the response recorded in the file rather than sending the
//! request to the upstream server.
//!
//! The snapshot file can be manually edited to manipulate responses for testing purposes, or to
//! redact information that you don't want to include in source-controlled snapshot files.
//!
//! Requests are matched to snapshots based on the URL path, HTTP method, and base64 encoding of
//! the request body (if there is one). If the snapshot specifies the `path` field, the URL path
//! must match exactly. Alternatively, a snapshot containing a `regex` field will match any URL
//! path that matches the regular expression. A snapshot with an exact `path` match takes
//! precedence over a snapshot with `regex`. Snapshots recorded by the server will always specify
//! `path`. The only way to use `regex` is to manually edit a snapshot file. A typical pattern is
//! to record a snapshot from a REST API, then manually change `path` to `regex` and replace the
//! variable part of the path with `.*`. Note that any special characters in the path that have
//! meaning to the `regex` crate must be escaped with `\\`, such as the `?` in a URL query
//! parameter.
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
//! should not be used with APIs that return sensitive data.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::extract::Path as AxumPath;
use axum::extract::RawQuery;
use axum::extract::State;
use axum::routing::any;
use base64::Engine;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::Method;
use http::Uri;
use http::header::CONNECTION;
use http::header::CONTENT_LENGTH;
use http::header::HOST;
use http::header::TRAILER;
use http::header::TRANSFER_ENCODING;
use http::header::UPGRADE;
use hyper::StatusCode;
use hyper_rustls::ConfigBuilderExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use serde_json_bytes::json;
use tokio::net::TcpListener;
use tower::ServiceExt;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::configuration::shared::Client;
use crate::services::http::HttpClientService;
use crate::services::http::HttpRequest;
use crate::services::router;
use crate::services::router::body::RouterBody;

/// Headers that will not be passed on to the upstream API
static FILTERED_HEADERS: [HeaderName; 6] = [
    CONNECTION,
    TRAILER,
    TRANSFER_ENCODING,
    UPGRADE,
    HOST,
    HeaderName::from_static("keep-alive"),
];

/// An error from the snapshot server
#[derive(Debug, thiserror::Error)]
enum SnapshotError {
    /// Unable to load snapshots
    #[error("unable to load snapshot file - {0}")]
    IoError(#[from] std::io::Error),
    /// Unable to parse snapshots
    #[error("unable to parse snapshots - {0}")]
    ParseError(#[from] serde_json::Error),
    /// Invalid snapshot
    #[error("invalid snapshot - {0}")]
    InvalidSnapshot(String),
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
    snapshots_by_key: Arc<Mutex<BTreeMap<String, Snapshot>>>,
    snapshots_by_regex: Arc<Mutex<Vec<Snapshot>>>,
    snapshot_file: Box<Path>,
    offline: bool,
    update: bool,
    include_headers: Option<Vec<String>>,
}

async fn root_handler(
    State(state): State<SnapshotServerState>,
    req: http::Request<axum::body::Body>,
) -> Result<http::Response<RouterBody>, StatusCode> {
    handle(State(state), req, "/".to_string(), None).await
}

async fn handler(
    State(state): State<SnapshotServerState>,
    AxumPath(path): AxumPath<String>,
    RawQuery(query): RawQuery,
    req: http::Request<axum::body::Body>,
) -> Result<http::Response<RouterBody>, StatusCode> {
    handle(State(state), req, path, query).await
}

async fn handle(
    State(state): State<SnapshotServerState>,
    req: http::Request<axum::body::Body>,
    path: String,
    query: Option<String>,
) -> Result<http::Response<RouterBody>, StatusCode> {
    let path = if let Some(query) = query {
        format!("{path}?{query}")
    } else {
        path
    };
    let uri = [state.base_url.to_string(), path.clone()].concat();
    let method = req.method().clone();
    let version = req.version();
    let request_headers: HeaderMap = req
        .headers()
        .clone()
        .drain()
        .filter_map(|(name, value)| {
            name.and_then(|name| {
                if !FILTERED_HEADERS.contains(&name) {
                    Some((name, value))
                } else {
                    None
                }
            })
        })
        .collect();
    let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .unwrap();
    let request_json_body = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

    let key = snapshot_key(
        Some(method.as_str()),
        Some(path.as_str()),
        &request_json_body,
    );

    if let Some(response) = response_from_snapshot(&state, &uri, &method, &key) {
        Ok(response)
    } else if state.offline && !state.update {
        fail(
            uri,
            method,
            "Offline mode enabled and no snapshot available",
        )
    } else {
        debug!(
            url = %uri,
            method = %method,
            "Taking snapshot"
        );
        let mut request = http::Request::builder()
            .method(method.clone())
            .version(version)
            .uri(uri.clone())
            .body(router::body::from_bytes(body_bytes))
            .unwrap();
        *request.headers_mut() = request_headers;
        let response = state
            .client
            .oneshot(HttpRequest {
                http_request: request,
                context: crate::context::Context::new(),
            })
            .await
            .unwrap();
        let (parts, body) = response.http_response.into_parts();

        if let Ok(body_bytes) = router::body::into_bytes(body).await {
            if let Ok(response_json_body) = serde_json::from_slice(&body_bytes) {
                let snapshot = Snapshot {
                    request: Request {
                        method: Some(method.to_string()),
                        path: Some(path),
                        regex: None,
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
                    let mut snapshots_by_key = state.snapshots_by_key.lock();
                    let mut snapshots_by_regex = state.snapshots_by_regex.lock();
                    snapshots_by_key.insert(key, snapshot.clone());
                    if let Err(e) = save(
                        state.snapshot_file,
                        &mut snapshots_by_key,
                        &mut snapshots_by_regex,
                    ) {
                        error!(
                            url = %uri,
                            method = %method,
                            error = ?e,
                            "Unable to save snapshot"
                        );
                    }
                }
                if let Ok(response) = snapshot.into_response() {
                    Ok(response)
                } else {
                    fail(uri, method, "Unable to convert snapshot into response body")
                }
            } else {
                fail(uri, method, "Unable to parse response body as JSON")
            }
        } else {
            fail(uri, method, "Unable to read response body")
        }
    }
}

fn response_from_snapshot(
    state: &SnapshotServerState,
    uri: &String,
    method: &Method,
    key: &String,
) -> Option<http::Response<RouterBody>> {
    let mut snapshots_by_key = state.snapshots_by_key.lock();
    let snapshots_by_regex = state.snapshots_by_regex.lock();
    if state.update {
        snapshots_by_key.remove(key);
        None
    } else {
        snapshots_by_key
            .get(key)
            .inspect(|snapshot| {
                debug!(
                    url = %uri,
                    method = %method,
                    path = %snapshot.request.path.as_ref().unwrap_or(&String::from("")),
                    "Found existing snapshot"
                );
            })
            .or_else(|| {
                // Look up snapshot using regex
                for snapshot in snapshots_by_regex.iter() {
                    if let Some(regex) = &snapshot.request.regex
                        && regex.is_match(uri)
                    {
                        debug!(
                            url = %uri,
                            method = %method,
                            regex = %regex.to_string(),
                            "Found existing snapshot"
                        );
                        return Some(snapshot);
                    }
                }
                None
            })
            .and_then(|snapshot| {
                snapshot
                    .clone()
                    .into_response()
                    .map_err(|e| error!("Unable to convert snapshot into HTTP response: {:?}", e))
                    .ok()
            })
    }
}

fn fail(
    uri: String,
    method: Method,
    message: &str,
) -> Result<http::Response<RouterBody>, StatusCode> {
    error!(
        url = %uri,
        method = %method,
        message
    );
    http::Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(router::body::from_bytes(
            json!({ "error": message}).to_string(),
        ))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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

fn save<P: AsRef<Path>>(
    path: P,
    snapshots_by_key: &mut BTreeMap<String, Snapshot>,
    snapshots_by_regex: &mut [Snapshot],
) -> Result<(), SnapshotError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let snapshots = snapshots_by_key
        .values()
        .cloned()
        .chain(snapshots_by_regex.iter().cloned())
        .collect::<Vec<_>>();
    std::fs::write(path, serde_json::to_string_pretty(&snapshots)?).map_err(Into::into)
}

fn load<P: AsRef<Path>>(
    path: P,
) -> Result<(BTreeMap<String, Snapshot>, Vec<Snapshot>), SnapshotError> {
    let str = std::fs::read_to_string(path)?;
    let snapshots: Vec<Snapshot> = serde_json::from_str(&str)?;
    let mut snapshots_by_key: BTreeMap<String, Snapshot> = Default::default();
    let mut snapshots_by_regex: Vec<Snapshot> = Default::default();
    for snapshot in snapshots.into_iter() {
        if snapshot.request.regex.is_some() {
            if snapshot.request.path.is_some() {
                return Err(SnapshotError::InvalidSnapshot(String::from(
                    "snapshot cannot specify both regex and path",
                )));
            }
            snapshots_by_regex.push(snapshot);
        } else {
            snapshots_by_key.insert(snapshot.key(), snapshot);
        }
    }
    Ok((snapshots_by_key, snapshots_by_regex))
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
        port: Option<u16>,
    ) -> Self {
        let listener = match port {
            Some(port) => Some(
                TcpListener::bind(format!("127.0.0.1:{port}"))
                    .await
                    .expect("Failed to bind an OS port for snapshot server"),
            ),
            None => None,
        };
        Self::inner_start(
            snapshot_path,
            base_url,
            true,
            offline,
            update,
            include_headers,
            listener,
        )
        .await
    }

    /// Start the server and block. Can be used to run the server as a standalone application.
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

        let (snapshots_by_key, snapshots_by_regex) = match load(snapshot_file) {
            Err(SnapshotError::IoError(ioe)) if ioe.kind() == std::io::ErrorKind::NotFound => {
                if offline {
                    warn!("Snapshot file not found in offline mode - all requests will fail");
                } else {
                    info!("Snapshot file not found - new snapshot file will be recorded");
                }
                (BTreeMap::default(), vec![])
            }
            Err(e) => {
                if offline {
                    warn!(
                        "Unable to load snapshot file in offline mode - all requests will fail: {e}"
                    );
                } else {
                    warn!("Unable to load snapshot file - new snapshot file will be recorded: {e}");
                }
                (BTreeMap::default(), vec![])
            }
            Ok((snapshots_by_key, snapshots_by_regex)) => {
                info!(
                    "Loaded {} snapshots",
                    snapshots_by_key.len() + snapshots_by_regex.len()
                );
                (snapshots_by_key, snapshots_by_regex)
            }
        };

        let http_service = HttpClientService::new(
            "test",
            rustls::ClientConfig::builder()
                .with_native_roots()
                .expect("Able to load native roots")
                .with_no_client_auth(),
            Client::builder().build(),
        )
        .expect("can create a HttpService");
        let app = Router::new()
            .route("/", any(root_handler))
            .route("/{*path}", any(handler)) // won't match root, so we need the root handler above
            .with_state(SnapshotServerState {
                client: http_service,
                base_url: base_url.clone(),
                snapshots_by_key: Arc::new(Mutex::new(snapshots_by_key)),
                snapshots_by_regex: Arc::new(Mutex::new(snapshots_by_regex)),
                snapshot_file: Box::from(snapshot_file),
                offline,
                update,
                include_headers,
            });
        let listener = match listener {
            Some(listener) => listener,
            None => TcpListener::bind("127.0.0.1:0")
                .await
                .expect("Failed to bind an OS port for snapshot server"),
        };
        let local_address = listener
            .local_addr()
            .expect("Failed to get snapshot server address.");
        info!(
            "Snapshot server listening on port {:?}",
            local_address.port()
        );
        if spawn {
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
        } else {
            axum::serve(listener, app).await.unwrap();
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

impl Snapshot {
    fn into_response(self) -> Result<http::Response<RouterBody>, ()> {
        let mut response = http::Response::builder().status(self.response.status);
        let body_string = self.response.body.to_string();
        if let Some(headers) = response.headers_mut() {
            for (name, values) in self.response.headers.into_iter() {
                if let Ok(name) = HeaderName::from_str(&name.clone()) {
                    for value in values {
                        if let Ok(value) = HeaderValue::from_str(&value.clone()) {
                            headers.insert(name.clone(), value);
                        }
                    }
                } else {
                    warn!("Invalid header name `{}` in snapshot", name);
                }
            }

            // Rewrite the content length header to the actual body length. Serializing and
            // deserializing the snapshot may result in a different length due to formatting
            // differences.
            headers.insert(CONTENT_LENGTH, HeaderValue::from(body_string.len()));
        }
        if let Ok(response) = response.body(router::body::from_bytes(body_string)) {
            return Ok(response);
        }
        Err(())
    }

    fn key(&self) -> String {
        snapshot_key(
            self.request.method.as_deref(),
            self.request.path.as_deref(),
            &self.request.body,
        )
    }
}

fn snapshot_key(method: Option<&str>, path: Option<&str>, body: &Value) -> String {
    if body.is_null() {
        format!("{}-{}", method.unwrap_or("GET"), path.unwrap_or("/"))
    } else {
        let body = base64::engine::general_purpose::STANDARD.encode(body.to_string());
        format!(
            "{}-{}-{}",
            method.unwrap_or("GET"),
            path.unwrap_or("/"),
            body,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Request {
    method: Option<String>,
    path: Option<String>,
    #[serde(with = "serde_regex", skip_serializing_if = "Option::is_none", default)]
    regex: Option<Regex>,
    body: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Response {
    status: u16,
    #[serde(default)]
    headers: IndexMap<String, Vec<String>>,
    body: Value,
}

/// Standalone snapshot server
pub(crate) mod standalone {
    use std::path::PathBuf;

    use clap::Parser;
    use http::Uri;
    use tokio::net::TcpListener;
    use tracing_core::Level;

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

        /// Turn on verbose output
        #[arg(short = 'v', long)]
        verbose: bool,
    }

    /// Run the snapshot server as a standalone application
    pub async fn main() {
        let args = Args::parse();

        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(if args.verbose {
                Level::DEBUG
            } else {
                Level::INFO
            })
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");

        let listener = match args.port {
            Some(port) => Some(
                TcpListener::bind(format!("127.0.0.1:{port}"))
                    .await
                    .expect("Failed to bind an OS port for snapshot server"),
            ),
            None => None,
        };

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
